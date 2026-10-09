//! The `Commands` subcommand enum — moved verbatim from `cli/mod.rs` (CAD-984).

use super::*;

#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Retain, inspect or explicitly send a local result to hosted queued custody.
    Remote {
        #[command(subcommand)]
        action: RemoteAction,
    },
    /// Authenticate to an AgenticOS issuer and map the workspace to a remote
    /// org (CAD-1019). `--slug` names the board host; the issuer's grant must
    /// echo exactly the workspace and audience requested.
    Login {
        /// AgenticOS HTTPS issuer origin (no path) — must match the
        /// operator-pinned trusted issuer.
        #[arg(long)]
        issuer: String,
        /// Exact AgenticOS workspace ID to authorize.
        #[arg(long)]
        org: String,
        /// Org slug: the board host is `https://<slug>.cadencecloud.app`.
        /// Required for org login (not for `--token-stdin`).
        #[arg(long)]
        slug: Option<String>,
        /// Select the new org as the saved default. Without it, login records
        /// the org but keeps an existing default.
        #[arg(long = "use")]
        use_: bool,
        /// Read an existing credential from stdin; never accept secrets in argv.
        #[arg(long)]
        token_stdin: bool,
        /// Print the authorization URL/code without opening a browser.
        #[arg(long, conflicts_with = "token_stdin")]
        no_open: bool,
        /// Isolated credential directory; defaults to XDG_CONFIG_HOME/cadence/remote-auth.
        #[arg(long)]
        auth_dir: Option<PathBuf>,
    },
    /// Inspect or remove local AgenticOS credentials.
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    /// Select an organization/connection preference (CAD-1019). The saved
    /// default is a pointer only — running work stays pinned to where it
    /// started, and a managed caller (`CADENCE_ALIAS`) cannot move it.
    Org {
        #[command(subcommand)]
        action: crate::cli::org::OrgAction,
    },
    /// Check environment, storage and provider CLIs. `--host` instead
    /// runs the read-only host watchdog — disk free, provider store and
    /// WAL growth, per-user pipe pressure, orphaned processes from
    /// deleted worktrees, leaked temp dirs and stale worktrees, and the
    /// tailnet sign-in proof with its whole remedy chain — with exit
    /// 0 ok, 1 warn, 2 fail.
    Doctor {
        /// Run the host watchdog checks instead of the environment probe.
        #[arg(long)]
        host: bool,
        /// With --host, print the JSON report instead of text lines
        /// (plain doctor already prints JSON, so this is a no-op there).
        #[arg(long)]
        json: bool,
        /// With --host, list what could be freed — stale worktrees,
        /// per-lane target dirs, the shared cargo cache — with sizes
        /// and the command. Never deletes anything.
        #[arg(long, requires = "host")]
        reclaim_plan: bool,
    },
    /// ADR 0007 T1: the dedicated-agent-uid lane — provision the
    /// boundary once (explicitly, as root), audit it, or print the
    /// operator's runbook. Nothing here ever runs implicitly: no
    /// startup hook, no install path, no other verb reaches it.
    AgentUid {
        #[command(subcommand)]
        action: AgentUidAction,
    },
    /// First run, idempotent: create what is missing — state dir,
    /// tracker, skill, daemon, board — and report provider CLIs (version
    /// and sign-in), the master agent (CAD-339) and the operator login
    /// (CAD-313). Anything already present is reported `ok` and left
    /// untouched. Exit 1 when any check failed.
    Setup {
        /// One JSON object per check, one per line:
        /// `{check, status, detail, fix}`; status is ok, created,
        /// missing, failed or unknown; fix is a copy-paste command.
        #[arg(long)]
        json: bool,
        /// Board port [default: the persisted `ui.json` port, else 3010].
        #[arg(long)]
        port: Option<u16>,
        /// Do not open a browser. Setup never opens one yet — it prints
        /// the board URL; accepted so `install.sh` and INSTALL-AGENT can
        /// pass it today.
        #[arg(long)]
        no_open: bool,
    },
    /// Manage the persistent controller.
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// One durable rollout claim. The lease gates a build change and a
    /// schema crossing. A same-build `daemon stop` followed by
    /// `daemon start`, or a crash restart of the same build, stays
    /// lease-free; requiring the lease for that same-build
    /// `daemon restart` is advisory. Inside a cadence pane the holder
    /// is `$CADENCE_ALIAS`; outside a pane pass `--as <identity>`.
    #[command(hide = true)]
    Rollout {
        #[command(subcommand)]
        action: RolloutAction,
    },
    /// CAD-1024: the staging-delegation allowlist and grant store
    /// (operator-only register/delegate/revoke; delegations is a read).
    /// A grant admits nothing until the `delegate:` caller shape lands.
    Staging {
        #[command(subcommand)]
        action: StagingAction,
    },
    /// Back up the store with SQLite's online backup API. The running
    /// daemon is not blocked. The copy is integrity-checked, hashed and
    /// described by a manifest (schema, sha256, versions, repo remotes),
    /// then re-verified from disk. `--keep` prunes older backups with
    /// the same `--reason` in that directory, oldest by file-name stamp;
    /// the copy just written is never pruned, and a file is deleted only
    /// when it is `<manifest stem>.sqlite3` and matches its manifest's
    /// sha256 and size. Schedule `--reason nightly` from cron
    /// for the nightly week of copies. See docs/SESSION.md.
    Backup {
        /// Where the copy and manifest go [default: <state dir>/backups].
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Keep the newest N backups with this reason.
        #[arg(long, default_value_t = 7, value_parser = clap::value_parser!(u64).range(1..=1000))]
        keep: u64,
        /// Label for the copy and its retention group: manual, nightly,
        /// pre-update, … (a-z, 0-9, '-').
        #[arg(long, default_value = "manual")]
        reason: String,
    },
    /// Write a portable bundle (`cadence.sqlite3` + `manifest.json`) to
    /// a new directory. Only the store goes in: endpoint columns are
    /// nulled, every turn token and generation is redacted from every
    /// text cell (refused if any remain), freed pages dropped, and every
    /// text cell scanned for
    /// credential patterns. One blocking finding refuses the export and
    /// writes nothing; warnings pass. The bundle is not signed: its
    /// sha256 detects corruption, not tampering.
    Export {
        /// The bundle directory to create. It must not exist.
        #[arg(long)]
        out: PathBuf,
    },
    /// Restore a backup (its `.manifest.json`) or an export bundle (its
    /// directory) into the state dir. Refuses while a daemon holds the
    /// state dir, refuses a schema newer than this binary, and refuses to
    /// replace an existing store without `--force`. Repo paths are
    /// rewritten to the `--repo` checkout with the same origin remote.
    Restore {
        /// A backup manifest file, or an export bundle directory.
        source: PathBuf,
        /// A checkout on this host, matched to a recorded repo by its
        /// origin remote. Repeat for each repo.
        #[arg(long = "repo")]
        repos: Vec<PathBuf>,
        /// Replace an existing store. A verified `pre-restore` backup of
        /// it is taken into <state dir>/backups first.
        #[arg(long)]
        force: bool,
    },
    /// Manage registered agents.
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },
    /// Send durable messages to an agent.
    Message {
        #[command(subcommand)]
        action: MessageAction,
    },
    /// Launch a Devin agent. The default is the official terminal in an
    /// owned tmux pane (pty endpoint). `--cloud` instead opens a Devin
    /// Cloud session over the v3 API.
    /// `-r <session-slug>` resumes an existing Devin session, mirroring
    /// `devin -r`; without it a fresh session is launched and becomes
    /// addressable by its discovered slug. Once the endpoint is open this
    /// terminal attaches to the owned pane by default (`--detach` opts
    /// out; a non-TTY or nested-tmux launch prints the command instead).
    /// `--cloud` opens a Devin Cloud session instead of a pty. Launch
    /// params are one `--cloud-params` string of `key=value` pairs
    /// separated by `;` (repo, devin_mode, max_acu_limit, playbook_id,
    /// knowledge_id, secret_id, platform, tag, bypass_approval,
    /// attachment_url). Repeat a key to append it. Unset max_acu_limit
    /// uses 10. `--permission-mode`, `--bypass` and `--auto-ready` are
    /// refused with `--cloud`.
    Devin {
        /// Resume an existing Devin session by its native slug.
        #[arg(short = 'r', long)]
        resume: Option<String>,
        /// Do not attach this terminal to the owned pane once open.
        #[arg(long)]
        detach: bool,
        /// Working directory for the session [default: current directory].
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Routing alias [default: the resumed slug, else devin-<random>].
        #[arg(long)]
        alias: Option<String>,
        /// pm, worker, or reviewer. `reviewer` is who a delivery review
        /// may be routed to; it does not grant PM authority.
        #[arg(long, default_value = "worker")]
        role: String,
        /// Team role used only to look up a model default. Does not
        /// change runtime pm/worker authorization. `ops` is stored as
        /// `devops`.
        #[arg(long)]
        team_role: Option<String>,
        /// File with role instructions, embedded in the agent's briefing
        /// under a role-instructions section. Refused with
        /// `--no-bootstrap` — the briefing is their only delivery channel.
        #[arg(long)]
        instructions_file: Option<PathBuf>,
        /// Run the session in an isolated checkout:
        /// `git worktree add <repo>/.cadence/wt/<name> -b cadence/<name>`
        /// becomes the agent's cwd.
        #[arg(long)]
        worktree: Option<String>,
        /// Also enqueue the briefing as the agent's first durable
        /// message (standalone launches get the file + AGENTS.md
        /// silently by default).
        #[arg(long, conflicts_with = "no_bootstrap")]
        bootstrap: bool,
        /// Skip the briefing file and AGENTS.md block entirely.
        #[arg(long)]
        no_bootstrap: bool,
        /// Opt into verified auto-ready: the daemon probes the pane and
        /// self-claims the ready gate when the TUI is visibly idle
        /// (a human `agent ready` still wins).
        #[arg(long)]
        auto_ready: bool,
        /// Write the cadence marker block into the cwd repo's AGENTS.md
        /// once the endpoint opens (persisted, re-applied on resume).
        /// Off by default — launches leave the repo untouched.
        #[arg(long)]
        agents_md: bool,
        /// Devin permission mode: auto, accept-edits, smart or
        /// dangerous. Persisted and replayed on every launch/resume.
        /// Pty only — refused with `--cloud`.
        #[arg(long)]
        permission_mode: Option<String>,
        /// Shortcut for --permission-mode dangerous.
        #[arg(long, conflicts_with = "permission_mode")]
        bypass: bool,
        /// Open a Devin Cloud session instead of a local pty.
        #[arg(long)]
        cloud: bool,
        /// Semicolon-separated cloud create params
        /// (`repo=owner/name;devin_mode=fast`). Requires `--cloud`.
        #[arg(long, value_name = "PARAMS", requires = "cloud")]
        cloud_params: Option<String>,
    },
    /// Launch a Codex agent on a managed-ws endpoint, attachable by the
    /// official Codex TUI via `codex resume --remote`. This terminal
    /// runs that attach once the endpoint is up by default (`--detach`
    /// opts out; a non-TTY or nested-tmux launch prints the command).
    Codex {
        /// Do not attach this terminal once the endpoint is up.
        #[arg(long)]
        detach: bool,
        /// Working directory for the session [default: current directory].
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Routing alias [default: codex-<random>].
        #[arg(long)]
        alias: Option<String>,
        /// pm, worker, or reviewer. `reviewer` is who a delivery review
        /// may be routed to; it does not grant PM authority.
        #[arg(long, default_value = "worker")]
        role: String,
        /// Codex model id (for example, gpt-5.6-luna). The app-server
        /// model catalogue validates it when the endpoint opens.
        #[arg(long)]
        model: Option<String>,
        /// Use the provider's native model instead of a daemon default.
        /// This does not pass a model argument to the provider.
        #[arg(long, conflicts_with = "model")]
        provider_default_model: bool,
        /// Team role used only to look up a model default. Does not
        /// change runtime pm/worker authorization. `ops` is stored as
        /// `devops`.
        #[arg(long)]
        team_role: Option<String>,
        /// Codex reasoning effort. The selected model's advertised
        /// reasoning efforts are validated at open time.
        #[arg(long, value_parser = ["low", "medium", "high", "xhigh", "max", "ultra"])]
        effort: Option<String>,
        /// Codex approval policy sent on `thread/start`, stored in
        /// params and replayed on resume [default: never].
        #[arg(long, value_parser = clap::builder::PossibleValuesParser::new(
            registry::CODEX_APPROVAL_POLICIES.iter().copied()))]
        approval_policy: Option<String>,
        /// Seconds without any provider event before a turn is declared
        /// unknown [default: 900]. Liveness is activity-based — a turn
        /// that keeps streaming runs as long as it needs.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        turn_idle_secs: Option<u64>,
        /// Optional absolute turn cap in seconds — fences even a chatty
        /// turn. Unset by default.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        turn_max_secs: Option<u64>,
        /// Codex filesystem sandbox sent on `thread/start` [default:
        /// workspace-write — a cadence-launched worker is writable;
        /// read-only only when asked].
        #[arg(long, value_parser = clap::builder::PossibleValuesParser::new(
            registry::CODEX_SANDBOXES.iter().copied()))]
        sandbox: Option<String>,
        /// File with role instructions, sent natively as codex developer
        /// instructions and embedded in the agent's briefing under a
        /// role-instructions section (skipped with `--no-bootstrap`).
        #[arg(long)]
        instructions_file: Option<PathBuf>,
        /// Run the session in an isolated checkout:
        /// `git worktree add <repo>/.cadence/wt/<name> -b cadence/<name>`
        /// becomes the agent's cwd.
        #[arg(long)]
        worktree: Option<String>,
        /// Also enqueue the briefing as the agent's first durable
        /// message (standalone launches get the file + AGENTS.md
        /// silently by default).
        #[arg(long, conflicts_with = "no_bootstrap")]
        bootstrap: bool,
        /// Skip the briefing file and AGENTS.md block entirely.
        #[arg(long)]
        no_bootstrap: bool,
        /// Accepted for parity with `join --tui`; codex has no pty
        /// endpoint so this is always a clear error, not a clap one.
        #[arg(long)]
        tui: bool,
        /// Write the cadence marker block into the cwd repo's AGENTS.md
        /// once the endpoint opens (persisted, re-applied on resume).
        /// Off by default — launches leave the repo untouched.
        #[arg(long)]
        agents_md: bool,
    },
    /// Launch a Claude agent on a managed stream-json endpoint — one
    /// long-lived headless `claude -p` process per agent; each durable
    /// message is one turn on its stdin and the turn's `result` event
    /// completes the message. No attachable surface: watch
    /// `cadence events --follow` instead. `--tui` instead runs the
    /// interactive Claude Code terminal in an owned tmux pane — the
    /// same gated pty endpoint as `cadence devin` (`-r` resumes a
    /// Claude session id there).
    Claude {
        /// Run the interactive Claude terminal in an owned tmux pane
        /// instead of the managed headless endpoint.
        #[arg(long)]
        tui: bool,
        /// Resume an existing Claude session id (requires --tui).
        #[arg(short = 'r', long)]
        resume: Option<String>,
        /// Working directory for the session [default: current directory].
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Routing alias [default: claude-<random>].
        #[arg(long)]
        alias: Option<String>,
        /// pm, worker, or reviewer. `reviewer` is who a delivery review
        /// may be routed to; it does not grant PM authority.
        #[arg(long, default_value = "worker")]
        role: String,
        /// Model flag passed to the CLI (e.g. sonnet, haiku, opus).
        /// Unset = the provider default from the CLI's own settings.
        #[arg(long)]
        model: Option<String>,
        /// Use the provider's native model instead of a daemon default.
        /// This does not pass a model argument to the provider.
        #[arg(long, conflicts_with = "model")]
        provider_default_model: bool,
        /// Team role used only to look up a model default. Does not
        /// change runtime pm/worker authorization. `ops` is stored as
        /// `devops`.
        #[arg(long)]
        team_role: Option<String>,
        /// Reasoning effort passed as `--effort`. Replayed on resume;
        /// unset = the provider default.
        #[arg(long, value_parser = ["low", "medium", "high", "xhigh", "max"])]
        effort: Option<String>,
        /// Claude permission mode [default: manual]. Replayed on resume.
        #[arg(long)]
        permission_mode: Option<String>,
        /// Extra auto-allowed tool patterns (`--allowedTools`);
        /// repeatable. `Bash(cadence *)` is always included.
        #[arg(long)]
        allow: Vec<String>,
        /// Shortcut for --permission-mode bypassPermissions.
        #[arg(long, conflicts_with = "permission_mode")]
        bypass: bool,
        /// Broker tool-permission prompts through `agent requests` /
        /// `agent respond` instead of auto-denying them — the CLI's
        /// `--permission-prompt-tool` is pointed at a cadence MCP
        /// server. Managed endpoint only; refused with `--bypass`
        /// (moot) and `--tui` (a pane answers its own prompts).
        #[arg(long, conflicts_with_all = ["bypass", "tui"])]
        broker_approvals: bool,
        /// Seconds a brokered prompt waits on an operator decision
        /// before the tool call is denied [default: 900].
        #[arg(long, requires = "broker_approvals", value_parser = clap::value_parser!(u64).range(1..))]
        permission_timeout_secs: Option<u64>,
        /// Seconds without any provider event before a turn is declared
        /// unknown [default: 900]. Liveness is activity-based — a turn
        /// that keeps emitting events runs as long as it needs.
        /// Managed endpoint only.
        #[arg(long, conflicts_with = "tui", value_parser = clap::value_parser!(u64).range(1..))]
        turn_idle_secs: Option<u64>,
        /// Optional absolute turn cap in seconds — fences even a chatty
        /// turn. Unset by default. Managed endpoint only.
        #[arg(long, conflicts_with = "tui", value_parser = clap::value_parser!(u64).range(1..))]
        turn_max_secs: Option<u64>,
        /// File with role instructions, embedded in the agent's briefing
        /// under a role-instructions section. Refused with
        /// `--no-bootstrap` — the briefing is their only delivery channel.
        #[arg(long)]
        instructions_file: Option<PathBuf>,
        /// Run the session in an isolated checkout:
        /// `git worktree add <repo>/.cadence/wt/<name> -b cadence/<name>`
        /// becomes the agent's cwd.
        #[arg(long)]
        worktree: Option<String>,
        /// Also enqueue the briefing as the agent's first durable
        /// message (standalone launches get the file + AGENTS.md
        /// silently by default).
        #[arg(long, conflicts_with = "no_bootstrap")]
        bootstrap: bool,
        /// Skip the briefing file and AGENTS.md block entirely.
        #[arg(long)]
        no_bootstrap: bool,
        /// Opt into verified auto-ready (requires --tui): the daemon
        /// probes the pane and self-claims the ready gate when the TUI
        /// is visibly idle (a human `agent ready` still wins).
        #[arg(long, requires = "tui")]
        auto_ready: bool,
        /// Accepted for interface parity; managed endpoints never attach.
        #[arg(long)]
        detach: bool,
        /// Write the cadence marker block into the cwd repo's AGENTS.md
        /// once the endpoint opens (persisted, re-applied on resume).
        /// Off by default — launches leave the repo untouched.
        #[arg(long)]
        agents_md: bool,
    },
    /// Launch a Cursor Agent terminal (`cursor-agent`) as a managed
    /// agent on the pty endpoint — an owned tmux pane, the same gated
    /// transport as `cadence devin`. `-r <chatId>` resumes an existing
    /// Cursor chat; without it a fresh chat is minted with
    /// `cursor-agent create-chat` and becomes addressable by its id.
    /// Once the endpoint is open this terminal attaches to the owned
    /// pane by default (`--detach` opts out; a non-TTY or nested-tmux
    /// launch prints the command instead).
    Cursor {
        /// Resume an existing Cursor chat by its id.
        #[arg(short = 'r', long)]
        resume: Option<String>,
        /// Do not attach this terminal to the owned pane once open.
        #[arg(long)]
        detach: bool,
        /// Working directory for the session [default: current directory].
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Routing alias [default: cursor-<random>].
        #[arg(long)]
        alias: Option<String>,
        /// pm, worker, or reviewer. `reviewer` is who a delivery review
        /// may be routed to; it does not grant PM authority.
        #[arg(long, default_value = "worker")]
        role: String,
        /// Model flag passed to the CLI (`--model <model>`).
        #[arg(long)]
        model: Option<String>,
        /// Use the provider's native model instead of a daemon default.
        /// This does not pass a model argument to the provider.
        #[arg(long, conflicts_with = "model")]
        provider_default_model: bool,
        /// Team role used only to look up a model default. Does not
        /// change runtime pm/worker authorization. `ops` is stored as
        /// `devops`.
        #[arg(long)]
        team_role: Option<String>,
        /// Cursor permission mode: auto-review or force. Persisted and
        /// replayed on every launch/resume.
        #[arg(long)]
        permission_mode: Option<String>,
        /// Shortcut for --permission-mode force.
        #[arg(long, conflicts_with = "permission_mode")]
        bypass: bool,
        /// File with role instructions, embedded in the agent's briefing
        /// under a role-instructions section. Refused with
        /// `--no-bootstrap` — the briefing is their only delivery channel.
        #[arg(long)]
        instructions_file: Option<PathBuf>,
        /// Run the session in an isolated checkout:
        /// `git worktree add <repo>/.cadence/wt/<name> -b cadence/<name>`
        /// becomes the agent's cwd.
        #[arg(long)]
        worktree: Option<String>,
        /// Also enqueue the briefing as the agent's first durable
        /// message (standalone launches get the file + AGENTS.md
        /// silently by default).
        #[arg(long, conflicts_with = "no_bootstrap")]
        bootstrap: bool,
        /// Skip the briefing file and AGENTS.md block entirely.
        #[arg(long)]
        no_bootstrap: bool,
        /// Opt into verified auto-ready: the daemon probes the pane and
        /// self-claims the ready gate when the TUI is visibly idle
        /// (a human `agent ready` still wins).
        #[arg(long)]
        auto_ready: bool,
        /// Write the cadence marker block into the cwd repo's AGENTS.md
        /// once the endpoint opens (persisted, re-applied on resume).
        /// Off by default — launches leave the repo untouched.
        #[arg(long)]
        agents_md: bool,
    },
    /// Enqueue a durable message to an agent — the hot-path alias for
    /// `message send`. Returns once the message is durable; delivery and
    /// reporting continue asynchronously.
    ///
    /// `cadence send <ALIAS> --text <body>`: the recipient is the
    /// positional alias (or `--to <ALIAS>`, exactly one of the two) and
    /// the body is `--text`, `-m` or `--file`. There are no email-style
    /// `--subject`, `--body` or `--cc` flags.
    ///
    /// Multi-topic reports: open the body with a `SUBJECT: <topic>`
    /// line (a single-line pty body leads with `SUBJECT: <topic> —`)
    /// so the recipient can scan topics; there is no subject field.
    #[command(group(clap::ArgGroup::new("target").required(true).args(["alias", "to"])))]
    Send {
        /// Agent alias or provider-native id.
        alias: Option<String>,
        /// The same recipient as the positional alias; give exactly one.
        #[arg(long)]
        to: Option<String>,
        /// Literal single-line body.
        #[arg(short = 'm', long, conflicts_with = "file")]
        text: Option<String>,
        /// Read the body from a file.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Idempotency key; retries with the same id+content dedupe.
        #[arg(long)]
        message: Option<String>,
        /// Route the result to another agent when the turn finishes.
        #[arg(long)]
        reply_to: Option<String>,
        /// Attach this delivery to a task — ad-hoc follow-up inside a
        /// job's delivery record.
        #[arg(long)]
        task: Option<String>,
        /// Claim `agent ready` for the target first — the flag IS the
        /// operator's explicit claim; the claim probes the pane and
        /// refuses a visibly busy one. No-op on non-pty endpoints.
        #[arg(long)]
        ready: bool,
        /// Force the ready claim past a busy probe verdict.
        #[arg(long, requires = "ready")]
        force: bool,
        /// Mid-turn steering (pty only): paste into the live pane without
        /// owning a turn — passes the one-running-turn hold, owes no
        /// report, completes when the paste is confirmed, never replayed
        /// after a daemon restart. Live pane only; at most 500 chars;
        /// takes no `--reply-to` or `--task`.
        #[arg(long, conflicts_with_all = ["ready", "reply_to", "task", "priority", "supersedes"])]
        nudge: bool,
        #[command(flatten)]
        steer: SteerArgs,
    },
    /// One-step issue dispatch: `issue start` (idempotent, owner =
    /// the worker) then exactly one templated kickoff message, a
    /// tracker comment and a `message` ref on the issue. With `--job`
    /// the kickoff goes through `job dispatch` instead.
    Dispatch {
        /// Issue id (e.g. CAD-55).
        issue: String,
        /// Worker agent to dispatch to.
        #[arg(long)]
        to: String,
        /// Kickoff note the worker reads (`read <note> — …`) [default:
        /// the ticket's own issue.md].
        #[arg(long)]
        note: Option<PathBuf>,
        /// Worktree slug — default: the slugified issue title.
        #[arg(long)]
        name: Option<String>,
        /// Base ref — else the repo's origin/HEAD, else current branch.
        #[arg(long)]
        base: Option<String>,
        /// Repo path — same resolution as `issue start`.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Return address for the worker's result [default:
        /// CADENCE_ALIAS]. Required outside a cadence pane.
        #[arg(long)]
        reply_to: Option<String>,
        /// One-line summary for the kickoff body [default: issue
        /// title].
        #[arg(long)]
        summary: Option<String>,
        /// Open the M3 job and dispatch through `job dispatch`
        /// (requires --spec; the job's PM is --reply-to).
        #[arg(long, requires = "spec")]
        job: bool,
        /// Job spec file for --job — hashed at creation.
        #[arg(long)]
        spec: Option<PathBuf>,
        /// Do not inject matched project-memory lessons into the
        /// kickoff.
        #[arg(long)]
        no_lessons: bool,
        /// Dispatch even when the worker's pty pane cwd is outside
        /// every repo of the issue's project (CAD-202). The override
        /// is recorded on the issue. A deleted cwd still refuses.
        #[arg(long)]
        force: bool,
        /// Dispatch an issue that another PM or lane holds in
        /// doing/review (CAD-383). Without it such a dispatch is
        /// refused, naming the holder and the claim age. The reason is
        /// required and recorded on the issue.
        #[arg(long, value_name = "REASON")]
        take_over: Option<String>,
    },
    /// Join a new worker agent to a group. `<group>` is the PM agent —
    /// its alias or provider-native id — and `<provider>` is devin,
    /// codex, claude, pi, cursor or fake. The worker's results route back
    /// to the PM by default (its params gain `"upstream"`). This
    /// terminal attaches once the endpoint is open, same rules as
    /// `cadence devin`. `--cloud` (provider devin) opens a Devin Cloud
    /// session; `--cloud-params` is the same semicolon-separated
    /// `key=value` list as `cadence devin --cloud-params`. From inside
    /// an agent's pane only a group root (no upstream) may join, and
    /// only into its own group (CAD-149).
    Join {
        /// Group handle — the PM agent's alias or native session id.
        group: String,
        /// Worker provider: devin, codex, claude, pi, cursor or fake.
        provider: String,
        /// Resume an existing native session as the worker (devin slug,
        /// a Claude session id with --tui, or a Cursor chat id).
        #[arg(short = 'r', long)]
        resume: Option<String>,
        /// Run the worker's interactive terminal in an owned tmux pane
        /// — the provider's pty endpoint (claude: `cadence join <pm>
        /// claude --tui`; devin is always a TUI and needs no flag).
        #[arg(long)]
        tui: bool,
        /// Do not attach this terminal once the endpoint is up.
        #[arg(long)]
        detach: bool,
        /// Working directory for the worker [default: the group
        /// agent's cwd].
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Routing alias [default: <provider>-<random>].
        #[arg(long)]
        alias: Option<String>,
        /// pm, worker, or reviewer. `reviewer` is who a delivery review
        /// may be routed to; it does not grant PM authority.
        #[arg(long, default_value = "worker")]
        role: String,
        /// Worker filesystem sandbox, recorded on registration. Codex
        /// sends it on `thread/start`; for a codex worker the default
        /// is workspace-write — `read-only` only when asked. Other
        /// providers store the value without consuming it and keep the
        /// read-only default.
        #[arg(long, value_parser = ["read-only", "workspace-write"])]
        sandbox: Option<String>,
        /// File with role instructions, embedded in the worker's briefing
        /// under a role-instructions section (codex also receives them
        /// natively as developer instructions). Refused with
        /// `--no-bootstrap` on every provider but codex — the briefing is
        /// their only delivery channel there.
        #[arg(long)]
        instructions_file: Option<PathBuf>,
        /// Run the worker in an isolated checkout of the PM's repo:
        /// `git worktree add <repo>/.cadence/wt/<name> -b cadence/<name>`.
        #[arg(long)]
        worktree: Option<String>,
        /// Skip the briefing file, AGENTS.md block and bootstrap
        /// message entirely.
        #[arg(long)]
        no_bootstrap: bool,
        /// Opt the worker into verified auto-ready (pty providers only):
        /// the daemon probes the pane and self-claims when visibly idle.
        #[arg(long)]
        auto_ready: bool,
        /// Write the cadence marker block into the worker's repo
        /// AGENTS.md once the endpoint opens (persisted, re-applied on
        /// resume). Off by default — joins leave the repo untouched.
        #[arg(long)]
        agents_md: bool,
        /// Model flag for providers `claude`, `cursor`, `codex` and `pi`
        /// (e.g. sonnet, haiku, gpt-5.6-luna, anthropic/claude-sonnet-4).
        #[arg(long)]
        model: Option<String>,
        /// Use the provider's native model instead of a daemon default.
        /// This does not pass a model argument to the provider.
        #[arg(long, conflicts_with = "model")]
        provider_default_model: bool,
        /// Team role used only to look up a model default. Does not
        /// change runtime pm/worker authorization. `ops` is stored as
        /// `devops`.
        #[arg(long)]
        team_role: Option<String>,
        /// Reasoning effort for providers `claude`, `codex` and `pi`
        /// (`--effort`; pi's `set_thinking_level` vocabulary also takes
        /// `off` and `minimal`). Each provider's vocabulary is validated
        /// against the launch params at registration; for pi the
        /// adapter also verifies what stuck through `get_state`.
        #[arg(long, value_parser = ["off", "minimal", "low", "medium", "high", "xhigh", "max"])]
        effort: Option<String>,
        /// Codex approval policy, stored in params and replayed on
        /// resume [default: never]. Refused for other providers.
        #[arg(long, value_parser = clap::builder::PossibleValuesParser::new(
            registry::CODEX_APPROVAL_POLICIES.iter().copied()))]
        approval_policy: Option<String>,
        /// Permission mode, replayed on resume. Claude takes its own
        /// modes [default: manual]; devin takes auto, accept-edits,
        /// smart or dangerous; cursor takes auto-review or force.
        /// Refused with `--cloud`.
        #[arg(long)]
        permission_mode: Option<String>,
        /// Extra auto-allowed tool patterns for provider `claude`;
        /// repeatable. `Bash(cadence *)` is always included.
        #[arg(long)]
        allow: Vec<String>,
        /// Permission-mode shortcut: bypassPermissions for claude,
        /// dangerous for devin, force for cursor.
        #[arg(long, conflicts_with = "permission_mode")]
        bypass: bool,
        /// Seconds without any provider event before a claude, codex or
        /// pi turn is declared unknown [default: 900]. Managed endpoint only.
        #[arg(long, conflicts_with = "tui", value_parser = clap::value_parser!(u64).range(1..))]
        turn_idle_secs: Option<u64>,
        /// Optional absolute turn cap in seconds for provider `claude`,
        /// `codex` or `pi`. Managed endpoint only.
        #[arg(long, conflicts_with = "tui", value_parser = clap::value_parser!(u64).range(1..))]
        turn_max_secs: Option<u64>,
        /// Broker a claude worker's tool-permission prompts through
        /// `agent requests` / `agent respond` instead of auto-denying
        /// them. Managed endpoint only; refused with `--bypass` and
        /// `--tui`.
        #[arg(long, conflicts_with_all = ["bypass", "tui"])]
        broker_approvals: bool,
        /// Seconds a brokered prompt waits on an operator decision
        /// before the tool call is denied [default: 900].
        #[arg(long, requires = "broker_approvals", value_parser = clap::value_parser!(u64).range(1..))]
        permission_timeout_secs: Option<u64>,
        /// Open a Devin Cloud session instead of a local pty. Provider
        /// must be `devin`.
        #[arg(long)]
        cloud: bool,
        /// Semicolon-separated cloud create params. Requires `--cloud`.
        #[arg(long, value_name = "PARAMS", requires = "cloud")]
        cloud_params: Option<String>,
        /// Landlock-confine the worker (provider `pi`, CAD-556): the
        /// child sees only its worktree, the repo's shared git dir and
        /// dep caches, the toolchain, its own state dir and the PM
        /// tracker — `agent show` prints the emitted policy. Refused
        /// on a host without Landlock; unset defers to the pm.yaml
        /// `[host] confine_pi_workers` default.
        #[arg(long)]
        confine: bool,
        /// Explicitly unconfined — overrides a `[host]
        /// confine_pi_workers` default for this one worker.
        #[arg(long, conflicts_with = "confine")]
        no_confine: bool,
    },
    /// Attach this terminal to a live agent's native endpoint. `name`
    /// may be an alias, a provider-native id, or a provider name when
    /// exactly one live agent of that provider exists. With no name,
    /// lists live attachable agents — attaching only when exactly one
    /// exists (never guesses).
    Attach {
        /// Agent alias, provider-native id, or provider name.
        name: Option<String>,
        /// Print the attach command instead of exec'ing it.
        #[arg(long)]
        print: bool,
    },
    /// Resume a group: the PM first, then every member whose
    /// `params.upstream` names it and that lacks a live endpoint —
    /// then attach this terminal to the PM pane by default (`--detach`
    /// opts out; non-TTY or nested tmux prints the attach command).
    /// `--all` instead sweeps every resumable registered agent.
    Resume {
        /// Group handle — the PM agent's alias or provider-native id.
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        group: Option<String>,
        /// Resume every agent with a stored thread/session and no live
        /// endpoint.
        #[arg(long)]
        all: bool,
        /// Do not attach once the group is up.
        #[arg(long)]
        detach: bool,
    },
    /// Stop a whole group — the PM and every member. Agents stay
    /// registered and resumable; `agent stop` stays single-agent.
    Stop {
        /// Group handle — the PM agent's alias or provider-native id.
        group: String,
    },
    /// Stop an agent's running turn with the provider's own interrupt —
    /// Claude's stream-json interrupt request, Codex `turn/interrupt`, a
    /// pane's interrupt key — never a kill. The message finishes
    /// `interrupted` (held text and partial tool results recorded), the
    /// agent stays up and idle for its next message, and nothing is
    /// replayed. Only the operator or the agent's own PM may interrupt
    /// it. No running turn is a recorded no-op.
    Interrupt {
        /// Agent alias or provider-native id.
        alias: String,
        /// Seconds to wait for the turn to settle before answering
        /// (max 120; 0 answers as soon as the interrupt is sent).
        #[arg(long, default_value_t = 30)]
        wait: u64,
    },
    /// Inside a cadence-owned pane: print this agent's alias, its
    /// running message id and the report token for it. Errors when
    /// `CADENCE_ALIAS` is absent (not a cadence pane). For an inbox
    /// alias (set by hand in an outside terminal) it prints the queued
    /// inbound count instead.
    #[command(name = "self")]
    SelfInfo,
    /// Report your running turn's result.
    ///
    /// `message result` with no ids: the daemon resolves your single
    /// running turn from the connection itself. With an explicit id (a
    /// unique 8+ char prefix works) plus `--token`, it reports exactly
    /// that message. The body is `--text` or `--file` (`-` reads stdin,
    /// capped at 4 MiB).
    Done {
        /// Message id; omit it and `--token` together to report your
        /// single running turn.
        message: Option<String>,
        /// Submission token (the `turn_id` `cadence self` prints).
        #[arg(long)]
        token: Option<String>,
        /// Result text reported for the message.
        #[arg(long, conflicts_with = "file")]
        text: Option<String>,
        /// Read the result text from a file (`-` reads stdin).
        #[arg(long)]
        file: Option<PathBuf>,
        /// The commit this report produced — binds the message to an
        /// exact revision for `job verdict`.
        #[arg(long)]
        sha: Option<String>,
        /// A task report file (`cadence.report/2`, see `cadence report
        /// file`) whose frontmatter names kind and task. It is filed on
        /// the ticket first — a malformed report refuses the result —
        /// and the result text gains a `Report: <ID>/reports/<file>`
        /// line. A retry with the same file reuses the stored report.
        #[arg(long)]
        report: Option<PathBuf>,
    },
    /// Read an inbox agent's durable queue: one JSON object per
    /// message, oldest first. The default drains — each message is
    /// marked completed `via=inbox_read` as it is printed. The safe
    /// mode (CAD-480) is `--peek` plus `inbox ack`: peek changes no
    /// state, so a reader that crashes or truncates loses nothing, and
    /// each reader's server-side cursor resumes it after its last ack.
    /// `--follow` blocks on the daemon for new arrivals — a waiting
    /// consumer needs no polling loop.
    Inbox {
        /// Inbox agent alias or provider-native id.
        alias: Option<String>,
        #[command(subcommand)]
        action: Option<InboxAction>,
        /// Read without consuming: messages stay `queued` for the next
        /// reader. Without `--after` the reader resumes after its own
        /// last ack (the server-side cursor for `--reader`).
        #[arg(long)]
        peek: bool,
        /// Only consume or peek messages after this sequence cursor.
        /// Peek mode defaults to the reader's ack watermark; a drain
        /// defaults to 0 (everything queued).
        #[arg(long)]
        after: Option<i64>,
        /// Seconds to wait for new messages per request (0-30).
        #[arg(long, default_value_t = 0)]
        wait: u64,
        /// Keep reading new arrivals until interrupted.
        #[arg(long)]
        follow: bool,
        /// Reader name for the server-side ack cursor. Readers that
        /// share a name share a cursor (default "default").
        #[arg(long)]
        reader: Option<String>,
        /// Push delivery: run CMD once per message, implying
        /// `--follow --peek`. Everything after `--exec` is the command's
        /// argv, run without a shell — write `--exec sh -c '…'` to ask
        /// for one (so `--exec` must come last). The message JSON is
        /// written to the command's stdin; it is acked only when the
        /// command exits 0. A non-zero exit leaves the message queued
        /// and retries it with bounded backoff. No message content is
        /// placed in argv or the environment.
        #[arg(long, num_args = 1.., allow_hyphen_values = true, value_name = "CMD")]
        exec: Option<Vec<String>>,
        /// First retry delay for a failed `--exec` run, in
        /// milliseconds; it doubles per failure up to 30 seconds.
        #[arg(long, default_value_t = 1000)]
        exec_retry_ms: u64,
        /// Per-message wall-clock limit for one `--exec` run, in
        /// milliseconds (default 120000 = 2 minutes). A run past the
        /// limit is killed and counted as a failure; 0 disables the
        /// limit.
        #[arg(long, default_value_t = 120_000)]
        exec_timeout_ms: u64,
        /// Consecutive failures on one message before it is parked:
        /// the follower records `inbox_park` for its reader, skips the
        /// message (it stays queued and unread) and moves on.
        #[arg(long, default_value_t = 5)]
        exec_max_failures: u32,
    },
    /// Install or inspect the `cadence` agent skill under
    /// `~/.agents/skills/cadence` with symlinks into the `.claude`,
    /// `.cursor` and `.copilot` skill dirs.
    Skill {
        #[command(subcommand)]
        action: SkillAction,
    },
    /// Read the durable event log for an agent.
    Events {
        /// Agent alias or provider-native id (Devin slug, Codex thread).
        /// Omit when --job names a job's scoped event view.
        alias: Option<String>,
        /// Read the job-scoped event view instead of one alias's log.
        #[arg(long)]
        job: Option<String>,
        /// Return events after this cursor. Omit for the newest page —
        /// the default shows the latest 50 events, oldest first, with
        /// the cursor to continue forward from.
        #[arg(long)]
        after: Option<i64>,
        /// Seconds to wait for new events per request (0-30).
        #[arg(long, default_value_t = 0)]
        wait: u64,
        /// Keep streaming new events until interrupted — starts from
        /// the newest page, not the beginning of the log.
        #[arg(long)]
        follow: bool,
    },
    /// Jobs: the work axis. A job binds a spec, a PM (group root or
    /// inbox) and tasks; each task revision is one kickoff message and
    /// a verdict binds QA to the exact reported commit. See docs/JOBS.md.
    Job {
        #[command(subcommand)]
        action: JobAction,
    },
    /// An agent's durable conversation thread (CAD-319): the operator's
    /// chat with it, outliving every provider session. Read-only here —
    /// the chat itself is the board's `/api/threads/<alias>`.
    Thread {
        #[command(subcommand)]
        action: ThreadAction,
    },
    /// A retained chat attachment (CAD-1168): `attachment read <id>`
    /// returns its bounded text (txt/md/csv). The operator reads any
    /// retained id; the master reads only an id on its own running
    /// turn's envelope.
    Attachment {
        #[command(subcommand)]
        action: AttachmentAction,
    },
    /// Daemon-owned persistent supervision registrations and local alerts.
    /// Monitor state describes the observer; delivery remains explicitly
    /// unconfigured in this bounded increment.
    Monitor {
        #[command(subcommand)]
        action: MonitorAction,
    },
    /// The issue board: folders under the PM dir (`~/pm` or
    /// `CADENCE_PM_DIR`); this CLI is the only writer.
    Issue {
        #[command(subcommand)]
        action: cadence_agent::issue::cli::IssueAction,
    },
    /// Connected platforms (CAD-366, ADR 0006 §5.3): credential custody
    /// and per-agent grants. The operator enrolls, grants, revokes and
    /// rotates; an agent reads its own grants. Credential bytes cross
    /// the control socket once, operator→daemon — nothing ever returns
    /// them.
    /// Exact provider account management, without app or agent grants.
    Connection {
        #[command(subcommand)]
        action: ConnectionAction,
    },
    /// Platform accounts (ADR 0006): the operator enrolls, rotates and
    /// revokes scoped credentials, grants agents scopes and sets
    /// per-project defaults; an agent reads only its own grants and
    /// effects.
    Platform {
        #[command(subcommand)]
        action: PlatformAction,
    },
    /// Plans (CAD-359/360): a proposed epic with its tickets. `propose`
    /// writes it from a Markdown file; the operator `approve`s or
    /// `reject`s it (operator connection only); until approved none of
    /// its tickets dispatch. `show` prints state, tickets and
    /// size-weighted progress.
    /// Idea pipeline (CAD-139): research and a plan can run, then the
    /// operator decides. Nothing is created until that decision.
    Idea {
        #[command(subcommand)]
        action: IdeaAction,
    },
    Plan {
        #[command(subcommand)]
        action: PlanAction,
    },
    /// Workflows (CAD-487): reusable plan files with `inputs:`,
    /// stored as `<pm>/<project>/workflows/<name>.md` beside
    /// PROJECT.md. `add`/`edit` are the only writers — one tracker
    /// commit each, `Actor:` recorded. `check` validates a file or a
    /// stored name. `approve` (operator only) pins the file's gate
    /// keys; `plan propose --workflow` renders it.
    Workflow {
        #[command(subcommand)]
        action: WorkflowAction,
    },
    /// Apps (CAD-547): an installable folder — `app.md` + `workflows/` +
    /// optional `rubrics/`, `templates/` — copied to
    /// `<pm>/<project>/apps/<name>/`. `install` validates every workflow
    /// and lands it unapproved; nothing in it runs until the operator's
    /// `approve` (the same gate and digest model as `workflow approve`).
    /// `set` binds each `needs.connections` slot to a connection name
    /// (default `local`); `update` prints the diff and re-gates on any
    /// structural change; `remove` refuses while a plan from the app is
    /// open.
    App {
        #[command(subcommand)]
        action: AppAction,
    },
    /// Projects (CAD-358): `new` registers a repo and seeds its
    /// PROJECT.md. The operator or the master (by its connection),
    /// through the daemon.
    Project {
        #[command(subcommand)]
        action: ProjectCmd,
    },
    /// Milestones (CAD-405): declared in the project's PROJECT.md or
    /// named by an issue's `milestone` / `m<n>-…` tag, with size-weighted
    /// progress and health rolled up from their epics and issues.
    Milestone {
        #[command(subcommand)]
        action: cadence_agent::issue::cli::MilestoneAction,
    },
    /// The master agent (CAD-339): one per install, alias `master` — the
    /// operator's assistant that proposes plans, dispatches approved
    /// tickets, routes questions and summarizes; it never implements.
    /// `start` has the daemon launch it from `agents/master/` (SOUL.md +
    /// AGENT.md under the PM dir, installed from defaults when missing);
    /// chat with it through its thread. `edit` is the one writer of its
    /// agent files. `summary` is the "since you left" digest. `start`
    /// and `edit` are operator only.
    Master {
        #[command(subcommand)]
        action: MasterAction,
    },
    /// The worker loop (CAD-431): a ticket the master dispatched goes to
    /// its worker; the worker's `done` report (`sha:` + `pr:`) is routed
    /// to an independent reviewer; the reviewer's `verdict` report sends
    /// a REVISE back (at most 2, then the operator decides) or a PASS
    /// on; a PASS on a green head is one "merge?" row in Needs-you.
    /// `ls` reads the loop. `sync`, `merge` and `decline` are the
    /// operator's and run GitHub (`gh`) with the operator's own
    /// credentials from this process — the daemon never does.
    Delivery {
        #[command(subcommand)]
        action: DeliveryAction,
    },
    /// File a report: a question, feedback, idea or bug becomes a
    /// tracker issue with context — instead of dying in a terminal
    /// scrollback. Routing is by kind, not by cwd: `question`,
    /// `feedback` and `bug` are about cadence itself and file into the
    /// `cadence` project from wherever you stand; `idea` belongs to the
    /// project being worked on — the cwd's repo project, or --project
    /// (which always wins). An `idea` with no resolvable project refuses
    /// rather than landing a tool bug in a product backlog. The issue is
    /// tagged `intake` plus the kind, lands in `backlog` (P3; `bug`
    /// defaults P2), and surfaces as an Overview `needs_me` row until it
    /// leaves backlog. One line also goes to the project's PM inbox when
    /// one is resolvable. Context (actor, cwd, repo+branch, cadence and
    /// daemon builds) is captured and credential-scrubbed. `--issue`
    /// files the same text as a comment on an existing issue instead.
    /// Exit 0 prints the issue id as JSON.
    Report {
        /// What this report is: question|feedback|idea|bug
        /// [default: feedback].
        #[arg(long, value_enum)]
        kind: Option<cadence_agent::issue::report::Kind>,
        /// Project key — always wins; required for `idea` when the cwd
        /// resolves to no known project.
        #[arg(long)]
        project: Option<String>,
        /// Attach the report as a comment on this issue instead of
        /// creating one — `--project`/`--priority` are unused here and
        /// rejected rather than silently ignored.
        #[arg(long, conflicts_with_all = ["project", "priority"])]
        issue: Option<String>,
        /// Inline report text — first line is the issue title.
        #[arg(short = 'm', conflicts_with = "file")]
        text: Option<String>,
        /// Read the report text from a file; else stdin.
        #[arg(long)]
        file: Option<PathBuf>,
        /// P0..P3 [default: P3; `bug` defaults P2].
        #[arg(long)]
        priority: Option<String>,
        #[command(subcommand)]
        action: Option<ReportAction>,
    },
    /// Relay local report issues to explicitly configured GitHub projects
    /// and poll actionable comments without model turns.
    Intake {
        #[command(subcommand)]
        action: IntakeAction,
    },
    /// Shared project memory: reviewed, scoped facts injected into
    /// dispatches and briefings. This CLI is the only writer.
    Memory {
        #[command(subcommand)]
        action: cadence_agent::memory::cli::MemoryAction,
    },
    /// The wiki: shared, scoped knowledge under the tracker's vault —
    /// git-backed text pages and content-addressed blobs (CAD-580).
    Wiki {
        #[command(subcommand)]
        action: cadence_agent::wiki::cli::WikiAction,
    },
    /// Credential scan (CAD-109): the check `issue comment`, `report`,
    /// `memory propose` and the intake relay run before they write.
    Secret {
        #[command(subcommand)]
        action: SecretAction,
    },
    /// The read-only board UI + JSON API on loopback.
    Ui {
        #[command(subcommand)]
        action: cadence_agent::ui::UiAction,
    },
    /// One-screen fleet overview: one row per agent with state, the
    /// running message's age and head, queued/unknown counts, pane
    /// verdict for pty agents, and owned tracker issues; a footer
    /// counts states and lists inboxes with unread messages. The
    /// `scope` field names which agents the rows cover.
    Status {
        /// Scope to one group root (default: the caller's group inside
        /// a cadence pane, else every registered agent).
        #[arg(long)]
        group: Option<String>,
        /// Show every agent even inside a cadence pane.
        #[arg(long, conflicts_with = "group")]
        all: bool,
        /// Emit the same data as JSON instead of the aligned table.
        #[arg(long)]
        json: bool,
        /// Re-render every <secs> until interrupted.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        watch: Option<u64>,
    },
    /// Queue a cargo test on the daemon (CAD-129). `submit` returns a
    /// job id; a content-addressed hit returns the earlier job instead
    /// of running again. `status`, `log` and `wait` follow that id.
    Test {
        #[command(subcommand)]
        action: test_cmd::TestAction,
    },
    /// Bounded, fair cargo build/test scheduling (CAD-113): the daemon
    /// grants a bounded number of concurrent build and suite slots —
    /// wrap `cargo build|test|clippy` so the host stays responsive
    /// under a fleet of agents.
    BuildSlot {
        #[command(subcommand)]
        action: BuildSlotAction,
    },
    /// Review a PR end-to-end: detached checkout under
    /// `.cadence/wt/review-<pr>` (the merge result when the base moved),
    /// config-driven gates from `cadence-review.toml` as committed on the
    /// base head (a PR that changes it is flagged and never suggested
    /// `pass`), new-test stress, one full-suite run, and an
    /// equal-conditions compare of every failure on the gated tree and
    /// the base head. Writes a
    /// Markdown+JSON report under the state dir — never posts a
    /// status, never merges, never pushes.
    #[command(subcommand_negates_reqs = true, args_conflicts_with_subcommands = true)]
    Review {
        #[command(subcommand)]
        action: Option<ReviewAction>,
        /// PR number (or anything `gh pr view` accepts).
        #[arg(required = true)]
        pr: Option<String>,
        /// owner/name — else resolved through `gh repo view`.
        #[arg(long)]
        repo: Option<String>,
        /// Run the full suite once (default).
        #[arg(long, conflicts_with = "no_full")]
        full: bool,
        /// Skip the full-suite run.
        #[arg(long)]
        no_full: bool,
        /// Run the full suite without the host-wide slot even though
        /// `CADENCE_SUITE_LOCK` is unset (refused otherwise).
        #[arg(long)]
        no_suite_lock: bool,
        /// Isolated stress runs per matched new test [default 5].
        #[arg(long, default_value_t = 5)]
        stress: u32,
        /// Keep the review worktree(s) for inspection.
        #[arg(long)]
        keep: bool,
        /// Print the JSON report on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Session bookends — `start` runs the morning go/no-go gate
    /// (host, binary, daemon, board, reconcile, inbox) and `end` runs
    /// the evening sweep plus the handoff note. See docs/SESSION.md.
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
    /// Reconstruct every merge on the default branch from stored
    /// data — verdict notes, commit statuses, tracker folders, daemon
    /// events, operator approval records — and flag merges with no
    /// passing verdict on the exact landed head, human-class merges with
    /// no operator approval for that head, and `reviewer==merger` where
    /// the fleet's identities differ. Read-only; exits non-zero when any
    /// row is flagged. `audit approve`/`audit revoke` record operator
    /// approval evidence (operator connection only). See docs/AUDIT.md.
    #[command(args_conflicts_with_subcommands = true)]
    Audit {
        #[command(subcommand)]
        action: Option<AuditAction>,
        /// Drop merges older than this: 24h, 7d, YYYY-MM-DD or epoch.
        #[arg(long)]
        since: Option<String>,
        /// Keep rows classified auto, notify or human.
        #[arg(long)]
        class: Option<String>,
        /// Keep rows whose tracker issue lives under project P.
        #[arg(long)]
        project: Option<String>,
        /// Emit the payload as one stable JSON document (cadence.audit/1).
        #[arg(long)]
        json: bool,
        /// Cap rows (0 = all; default 200).
        #[arg(long)]
        limit: Option<u64>,
        /// Audit this checkout instead of the cwd.
        #[arg(long, hide = true)]
        repo: Option<PathBuf>,
        /// Fixture the notes directory (default /var/www/agent-notes).
        #[arg(long, value_name = "PATH", hide = true)]
        notes_dir: Option<PathBuf>,
        /// Fixture replacing every `gh` call — the audit never shells
        /// out when this is set.
        #[arg(long, value_name = "PATH", hide = true)]
        merge_report: Option<PathBuf>,
    },
    /// What needs a human right now: merge-ready PRs, open approvals,
    /// fenced or stalled agents, review/unblocked issues, unread
    /// inboxes, a behind-tracker, deploy drift — each with the exact
    /// command. The same payload as the board's Overview screen. Rows
    /// naming the same agent, issue or PR merge into one row listing
    /// its causes.
    Overview {
        /// Emit the payload as JSON instead of the aligned list.
        #[arg(long)]
        json: bool,
        /// Re-render every <secs> until interrupted.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        watch: Option<u64>,
        /// Scope rows to one tracker project key (an unknown key is an
        /// error).
        #[arg(long)]
        project: Option<String>,
        /// Scope rows to one group root and its members, as
        /// `cadence status --group` does (an unknown root is an error).
        #[arg(long)]
        group: Option<String>,
    },
    /// Install the exact build CI tested on main (CAD-334). Downloads
    /// the `cadence-<sha>-x86_64-linux` artifact through `gh` and refuses
    /// unless the sha is on main, CI's `test` job passed on that exact sha,
    /// the sha256 matches, the manifest names the sha, and the GitHub
    /// build-provenance attestation verifies for this repo. Installs to
    /// `<releases>/<sha>/cadence` and atomically repoints the `cadence`
    /// symlink. A release already on disk is reinstalled without a
    /// download (rollback). Never restarts the daemon unless `--restart`.
    #[command(hide = true, group(clap::ArgGroup::new("upgrade_target").required(true).args(["sha", "latest_main"])))]
    Upgrade {
        /// Full 40-hex commit on main to install (or roll back to).
        #[arg(long)]
        sha: Option<String>,
        /// The newest main commit whose CI run succeeded.
        #[arg(long)]
        latest_main: bool,
        /// Verify everything and print the plan; install nothing.
        #[arg(long)]
        dry_run: bool,
        /// After installing, restart the daemon with the new binary
        /// through `daemon restart --when-idle --ui` (rollout lease).
        #[arg(long, conflicts_with = "dry_run")]
        restart: bool,
        /// Rollout identity for `--restart` outside a cadence pane.
        #[arg(long = "as", requires = "restart")]
        as_identity: Option<String>,
        /// With --sha: roll back to a release already on disk even though
        /// its attestation does not verify (a hand-built release). The
        /// report labels it an unattested local release.
        #[arg(long, requires = "sha")]
        allow_unattested: bool,
        /// GitHub repository whose CI built and attested the binary.
        /// An operator input: it decides which repository's builds are
        /// trusted.
        #[arg(long, default_value = cadence_agent::upgrade::DEFAULT_REPO)]
        repo: String,
        /// Symlink that puts cadence on PATH [default: ~/.local/bin/cadence].
        /// An operator input: it decides what gets replaced.
        #[arg(long)]
        link: Option<PathBuf>,
        /// Releases directory [default: read off the current link, else
        /// $XDG_DATA_HOME/cadence/releases]. An operator input: releases
        /// found there are candidates for reuse.
        #[arg(long)]
        releases_dir: Option<PathBuf>,
    },
    /// Update to the newest attested green main build in one command
    /// (CAD-561): check → verify attestation + hash → backup → install
    /// side by side → bounded drain → switch → health check → done.
    /// The rollout lease is auto-claimed and auto-released, and the
    /// pre-update backup (outside the state dir by default) is recorded
    /// as the lease receipt, so no manual copy step. `--check` shows
    /// what would happen and changes nothing; `--rollback` returns to
    /// the previous release; `status` shows a pending update and what
    /// it waits on. Operator-only: outside a pane with `--as`, never
    /// from inside one. `upgrade` and `rollout` stay the low-level
    /// commands.
    Update {
        #[command(subcommand)]
        action: Option<UpdateAction>,
        /// Show current vs available version, the merged PR titles
        /// between them, whether a schema migration is involved and
        /// what would block — changing nothing.
        #[arg(long)]
        check: bool,
        /// Return to the previous release (attested), restart, health
        /// check — and offer the backup restore when the schema changed.
        #[arg(long, conflicts_with = "check")]
        rollback: bool,
        /// Wait at most this long for in-flight turns before switching:
        /// 90s, 30m, 12h [default: 10m].
        #[arg(long, default_value = "10m")]
        drain: String,
        /// Switch immediately: no drain wait; interrupted turns resume
        /// after the restart.
        #[arg(long, conflicts_with_all = ["check", "rollback"])]
        now: bool,
        /// Previous releases kept under the releases dir [default: 3].
        #[arg(long, default_value_t = cadence_agent::update::DEFAULT_KEEP as u64,
              value_parser = clap::value_parser!(u64).range(0..))]
        keep: u64,
        /// Where the pre-update backup goes [default:
        /// `<state dir>/../cadence-backups`, outside the state dir].
        #[arg(long)]
        backup_dir: Option<PathBuf>,
        /// Machine-readable JSON (with the progress lines) instead of
        /// plain progress lines.
        #[arg(long)]
        json: bool,
        /// Append this run's progress lines and its final one-line JSON
        /// record to PATH (0600) — the board's Update card reads it while
        /// a detached helper runs (CAD-561). `--rollback` appends its
        /// lines there too; `--check` and `status` ignore it.
        #[arg(long, value_name = "PATH")]
        progress: Option<PathBuf>,
        /// Pin the target to this full 40-hex commit instead of the
        /// approved production candidate (CAD-1187). The same checks as
        /// `upgrade --sha` run: on main, CI `test` green on that sha,
        /// sha256, manifest and build-provenance attestation. With
        /// `--check` it reports that sha.
        #[arg(long, value_name = "SHA", conflicts_with = "rollback")]
        to: Option<String>,
        /// Where the release lives and which repository is trusted.
        #[command(flatten)]
        target: UpdateTargetArgs,
    },
    /// Your dev Cadence (CAD-1187; `sandbox` is the old name): a
    /// disposable Cadence beside production — `dev up`, rebuild,
    /// `dev reload --build <binary>` with no lease, attestation or
    /// backup. Its own state dir,
    /// tracker and board port under `$CADENCE_SANDBOX_ROOT` (default
    /// `$XDG_STATE_HOME/cadence-sandbox`), run from this binary with
    /// `CADENCE_PROFILE=sandbox:<name>` — which skips the skill sync
    /// into `$HOME`, refuses `ui tailscale`, and keeps the provider WAL
    /// watcher observe-only. Refuses production's dirs and port 3010.
    #[command(name = "dev", visible_alias = "sandbox")]
    Sandbox {
        #[command(subcommand)]
        action: cadence_agent::sandbox::SandboxAction,
    },
    /// Run a command under a filesystem sandbox (CAD-439): `--read`
    /// paths are readable and executable, `--write` paths fully usable,
    /// everything else denied. The daemon launches the master's
    /// provider through it.
    #[command(hide = true)]
    Confine {
        #[arg(long, value_name = "PATH")]
        read: Vec<PathBuf>,
        #[arg(long, value_name = "PATH")]
        write: Vec<PathBuf>,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Stdio MCP server backing `--permission-prompt-tool` on a
    /// brokered managed claude — spawned by the provider CLI via the
    /// generated `--mcp-config`, never by hand.
    #[command(hide = true)]
    McpPermission {
        /// Override the decision deadline in seconds (else
        /// `CADENCE_PERMISSION_TIMEOUT_SECS`, default 900).
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        timeout_secs: Option<u64>,
    },
    /// Stdio MCP tools for a managed Codex endpoint.
    #[command(hide = true)]
    McpAgent,
}
