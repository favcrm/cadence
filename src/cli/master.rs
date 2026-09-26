//! CAD-535: `cadence master` — moved verbatim from src/main.rs.

use super::*;

#[derive(Subcommand)]
pub(crate) enum MasterAction {
    /// Start the master: install any missing default agent file, then
    /// launch the managed session and queue its briefing. Provider,
    /// model and effort default to AGENT.md's `preferred`.
    Start {
        /// claude or pi [default: AGENT.md `preferred.provider`].
        #[arg(long)]
        provider: Option<String>,
        /// Model [default: AGENT.md's for that provider].
        #[arg(long)]
        model: Option<String>,
        /// Reasoning effort [default: AGENT.md's for that provider].
        #[arg(long)]
        effort: Option<String>,
        /// Only on a host that cannot confine the master (no Landlock:
        /// macOS, older kernels): start it WITHOUT the filesystem
        /// sandbox. It can then read and write your files; Needs-you
        /// shows it while it runs.
        #[arg(long)]
        unconfined: bool,
        /// Copy your login for the chosen provider (claude: the
        /// claudeAiOauth entry only; pi: its auth.json) — 0600, into the
        /// master's own config dir when it has none. Both then share one
        /// credential: a refresh or rotation on one side can sign the
        /// other out. The default is a separate login — `master start`
        /// prints the command.
        #[arg(long)]
        copy_login: bool,
    },
    /// Replace the master's SOUL.md or AGENT.md (operator only; one
    /// tracker commit). Takes effect at the next `master start`.
    Edit {
        /// SOUL.md or AGENT.md.
        name: String,
        /// The new content; `-` reads stdin.
        #[arg(long)]
        file: PathBuf,
    },
    /// The master's dispatch: hand a `ready` ticket of an approved plan,
    /// its blockers done, to the ticket's agent. The daemon composes and
    /// sends the kickoff; `--to` only when the ticket names no agent.
    /// Master only.
    Dispatch {
        /// The ticket id.
        issue: String,
        /// Target agent when the ticket names none.
        #[arg(long)]
        to: Option<String>,
    },
    /// Hand an open question the master cannot answer to the operator's
    /// Needs-you, with a summary (master or operator only).
    Escalate {
        /// The ticket id.
        issue: String,
        /// The question report's file name.
        question: String,
        /// The summary for the operator; `-` reads stdin.
        #[arg(long)]
        file: PathBuf,
    },
    /// "Since you left": plans proposed and decided, tickets moved,
    /// reports filed and open questions since a time.
    Summary {
        /// Epoch seconds, YYYY-MM-DDTHH:MM:SSZ, or a look-back (30m, 24h, 7d).
        #[arg(long, default_value = "24h")]
        since: String,
        /// Also post it into the master's thread.
        #[arg(long)]
        post: bool,
    },
    /// Print the filesystem confinement `master start` launches the
    /// master's provider under (CAD-439), computed from this env exactly
    /// as the daemon does: `{confine, read, write}`. Reads nothing,
    /// starts nothing — for `scripts/master-read-probe.sh`.
    #[command(hide = true)]
    Confinement,
    /// Ask the operator to approve one plain command (master only).
    /// The command is the exact argv after `--`.
    AskPermission {
        /// Why the master needs it (shown in Needs-you).
        #[arg(long)]
        reason: String,
        /// The exact command, after `--`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        argv: Vec<String>,
    },
    /// Does a live grant cover this exact command? Master only. The
    /// guard calls it for cadence verbs; exit 0 means yes. Read-only
    /// tools use `use-grant`, which consumes.
    #[command(hide = true)]
    PeekGrant {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        argv: Vec<String>,
    },
    /// Consume a single-use grant (or match an allow rule) and run the
    /// command. Master only. The guard calls this for `ls`/`cat`/`grep`/
    /// `find`, which never re-enter this CLI. Exit 0 only when applied.
    #[command(hide = true)]
    UseGrant {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        argv: Vec<String>,
    },
    /// Allow one pending request once (operator only).
    AllowOnce {
        /// The request id.
        id: String,
    },
    /// Save an allow rule for a pending request (operator only).
    AlwaysAllow {
        /// The request id.
        id: String,
        /// `exact` or `prefix`.
        #[arg(long, default_value = "exact")]
        scope: String,
        /// Argument patterns after the verb, for `--scope prefix`.
        /// A `*` is only legal at the end of an argument.
        #[arg(long = "arg")]
        arg: Vec<String>,
    },
    /// Reject a pending request (operator only).
    Reject {
        /// The request id.
        id: String,
        /// Also save a deny rule so this exact command is not asked again.
        #[arg(long)]
        dont_ask_again: bool,
    },
    /// Remove one permission rule (operator only). It stops matching
    /// immediately.
    RevokePermission {
        /// The rule id.
        id: String,
    },
    /// Pending requests and saved rules (operator only).
    Permissions,
}

pub(super) fn run_master(state_dir: &Path, action: MasterAction) -> Result<i32> {
    let result = match action {
        MasterAction::Start {
            provider,
            model,
            effort,
            unconfined,
            copy_login,
        } => {
            let out = client::rpc(
                state_dir,
                "master_start",
                json!({"provider": provider, "model": model, "effort": effort,
                       "unconfined": unconfined, "copy_login": copy_login}),
            )?;
            if let Some(w) = out["warning"].as_str() {
                eprintln!("WARNING: {w}");
            }
            if let Some(cmd) = out["login_command"].as_str() {
                // Name the provider the daemon actually resolved — a
                // bare `master start` lands on AGENT.md's preferred.
                let provider = out["provider"].as_str().unwrap_or("claude");
                eprintln!(
                    "The master has no {provider} login yet. Give it its own:\n  {cmd}\n\
                     (or `cadence master start --provider {provider} --copy-login` to copy yours)"
                );
            }
            out
        }
        MasterAction::Edit { name, file } => {
            let cap = (cadence_agent::master::AGENT_MAX_CHARS * 4) as u64;
            let text = read_body_capped(None, Some(file), cap)?;
            client::rpc(
                state_dir,
                "agent_file_write",
                json!({"agent": cadence_agent::master::ALIAS, "file": name, "text": text}),
            )?
        }
        MasterAction::Dispatch { issue, to } => client::rpc(
            state_dir,
            "master_dispatch",
            json!({"issue": issue, "to": to}),
        )?,
        MasterAction::Escalate {
            issue,
            question,
            file,
        } => {
            let summary = read_body_capped(None, Some(file), 4 * 4_000)?;
            client::rpc(
                state_dir,
                "question_escalate",
                json!({"issue": issue, "question": question, "summary": summary}),
            )?
        }
        MasterAction::Summary { since, post } => client::rpc(
            state_dir,
            "master_summary",
            json!({"since": since, "post": post}),
        )?,
        MasterAction::AskPermission { reason, argv } => {
            let cwd = std::env::current_dir().map_err(|e| {
                cadence_agent::error::Error::rejected(format!("cwd is unreadable: {e}"))
            })?;
            client::rpc(
                state_dir,
                "master_ask_permission",
                json!({"reason": reason, "argv": argv, "cwd": cwd}),
            )?
        }
        MasterAction::PeekGrant { argv } => {
            let cwd = std::env::current_dir().map_err(|e| {
                cadence_agent::error::Error::rejected(format!("cwd is unreadable: {e}"))
            })?;
            client::rpc(
                state_dir,
                "master_peek_grant",
                json!({"argv": argv, "cwd": cwd}),
            )?
        }
        MasterAction::UseGrant { argv } => {
            let cwd = std::env::current_dir().map_err(|e| {
                cadence_agent::error::Error::rejected(format!("cwd is unreadable: {e}"))
            })?;
            let out = client::rpc(
                state_dir,
                "master_permission_use",
                json!({"argv": argv, "cwd": cwd}),
            )?;
            let code = if out["applied"].as_bool() == Some(true) {
                0
            } else {
                1
            };
            print_json(&out);
            return Ok(code);
        }
        MasterAction::AllowOnce { id } => {
            client::rpc(state_dir, "master_permission_allow_once", json!({"id": id}))?
        }
        MasterAction::AlwaysAllow { id, scope, arg } => client::rpc(
            state_dir,
            "master_permission_always",
            json!({"id": id, "scope": scope, "tail": arg}),
        )?,
        MasterAction::Reject { id, dont_ask_again } => client::rpc(
            state_dir,
            "master_permission_reject",
            json!({"id": id, "dont_ask_again": dont_ask_again}),
        )?,
        MasterAction::RevokePermission { id } => {
            client::rpc(state_dir, "master_permission_revoke", json!({"id": id}))?
        }
        MasterAction::Permissions => client::rpc(state_dir, "master_permission_list", json!({}))?,
        MasterAction::Confinement => {
            let env = cadence_agent::adapter::ProviderEnv::default();
            let (confine, policy) =
                cadence_agent::adapter::claude::master_confinement(&env, state_dir);
            let (pi_confine, pi_policy) =
                cadence_agent::adapter::pi::pi_master_confinement(&env, state_dir);
            json!({
                "claude": {"confine": confine, "read": policy.read, "write": policy.write},
                "pi": {"confine": pi_confine, "read": pi_policy.read, "write": pi_policy.write},
            })
        }
    };
    print_json(&result);
    Ok(0)
}

pub(super) fn run(state_dir: PathBuf, action: MasterAction) -> Result<i32> {
    run_master(&state_dir, action)
}
