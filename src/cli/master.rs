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
            let text = if file.as_os_str() == "-" {
                read_body_capped(None, Some(file), cap)?
            } else {
                cadence_agent::master::read_command_file(state_dir, &file, cap)?
            };
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
            let summary = read_master_command_file(state_dir, Some(&file), 4 * 4_000)?;
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
