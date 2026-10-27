//! Agent, message and job dispatch — split from `action_dispatch.rs` (CAD-1654).

use super::*;

pub(super) fn dispatch(state_dir: PathBuf, command: Commands) -> Result<i32> {
    match command {
        Commands::Agent { action } => {
            if action.bridge().is_some() {
                Err(Error::usage(
                    "agent connect/watch/assignments/disconnect/reassign coordinate a hosted org with this machine. Select that org, or switch back from `--org local`. Nothing was claimed.",
                ))
            } else {
                agent::run(state_dir, action)
            }
        }
        Commands::Message { action } => message::run(state_dir, action),
        Commands::Endpoint { action } => crate::cli::endpoint::run(state_dir, action),
        Commands::Send {
            alias,
            to,
            text,
            file,
            message,
            reply_to,
            task,
            ready,
            force,
            nudge,
            steer,
            wait,
        } => {
            // The ArgGroup guarantees exactly one of the two.
            let recipient = alias.or(to).expect("clap target group");
            match wait {
                Some(secs) => message::run(
                    state_dir,
                    MessageAction::Ask {
                        alias: recipient,
                        text,
                        file,
                        message,
                        reply_to,
                        task,
                        ready,
                        force,
                        wait: secs,
                    },
                ),
                None => send::run(
                    state_dir, recipient, text, file, message, reply_to, task, ready, force,
                    nudge, steer,
                ),
            }
        }
        Commands::Dispatch {
            issue,
            to,
            note,
            name,
            base,
            repo,
            reply_to,
            summary,
            job,
            spec,
            no_lessons,
            force,
            take_over,
        } => dispatch::run(
            state_dir, issue, to, note, name, base, repo, reply_to, summary, job, spec, no_lessons,
            force, take_over,
        ),
        Commands::Join {
            group,
            provider,
            resume,
            tui,
            detach,
            cwd,
            alias,
            role,
            sandbox,
            instructions_file,
            worktree,
            no_bootstrap,
            auto_ready,
            agents_md,
            model,
            provider_default_model,
            team_role,
            effort,
            approval_policy,
            permission_mode,
            allow,
            bypass,
            turn_idle_secs,
            turn_max_secs,
            broker_approvals,
            permission_timeout_secs,
            cloud,
            cloud_params,
            confine,
            no_confine,
            profile,
        } => join::run(
            state_dir,
            group,
            provider,
            resume,
            tui,
            detach,
            cwd,
            alias,
            role,
            sandbox,
            instructions_file,
            worktree,
            no_bootstrap,
            auto_ready,
            agents_md,
            model,
            provider_default_model,
            team_role,
            effort,
            approval_policy,
            permission_mode,
            allow,
            bypass,
            turn_idle_secs,
            turn_max_secs,
            broker_approvals,
            permission_timeout_secs,
            cloud,
            cloud_params,
            confine,
            no_confine,
            profile,
        ),
        Commands::Attach { name, print } => attach::run(state_dir, name, print),
        Commands::Resume { group, all, detach } => resume::run(state_dir, group, all, detach),
        Commands::Stop { group } => stop::run(state_dir, group),
        Commands::Interrupt { alias, wait } => interrupt::run(state_dir, alias, wait),
        Commands::SelfInfo => {
            let alias = std::env::var("CADENCE_ALIAS").map_err(|_| {
                Error::rejected("CADENCE_ALIAS is not set — not inside a cadence-owned pane")
            })?;
            // CAD-879: only the running turns are read — no history.
            let show = client::rpc(&state_dir, "agent_show", self_show_params(&alias))?;
            // A mailbox has no running turn — report the inbound
            // backlog a consumer would drain instead.
            let (provider, kind) = (
                show["agent"]["provider"].as_str().unwrap_or_default(),
                show["agent"]["endpoint_kind"].as_str().unwrap_or_default(),
            );
            if !registry::has_actor(provider, kind) {
                // CAD-1514: the shape lives in `self_inbox_payload`, which
                // a test compares against the SKILL.md example.
                print_json(&self_inbox_payload(&show, kind));
                return Ok(0);
            }
            let payload = self_actor_payload(&alias, &show);
            // CAD-375: the daemon shows a running turn's token only to
            // the agent's own pane or endpoint. A `null` here means this
            // process is not in it — say so rather than print a report
            // instruction that cannot work.
            if payload["running"]
                .as_array()
                .is_some_and(|ms| ms.iter().any(|m| m["turn_id"].is_null()))
            {
                return Err(Error::rejected(format!(
                    "turn tokens for '{alias}' are shown only to that agent's own pane \
                     or managed endpoint, and this process does not descend from it — \
                     run `cadence self` inside the agent's pane (CAD-375)"
                )));
            }
            print_json(&payload);
            Ok(0)
        }
        Commands::Done {
            message,
            token,
            text,
            file,
            sha,
            report,
        } => {
            let (result, pending) =
                message::run_result(&state_dir, message, token, text, file, sha, report)?;
            print_json(&result);
            Ok(if pending { 2 } else { 0 })
        }
        Commands::Inbox {
            alias,
            action,
            peek,
            after,
            wait,
            follow,
            reader,
            exec,
            exec_retry_ms,
            exec_timeout_ms,
            exec_max_failures,
        } => inbox::run(
            state_dir,
            alias,
            action,
            peek,
            after,
            wait,
            follow,
            reader,
            exec,
            exec_retry_ms,
            exec_timeout_ms,
            exec_max_failures,
        ),
        Commands::Skill { action } => skill::run(state_dir, action),
        Commands::Events {
            alias,
            job,
            after,
            wait,
            follow,
        } => events::run(state_dir, alias, job, after, wait, follow),
        Commands::Job { action } => job::run(state_dir, action),
        Commands::Thread { action } => thread::run(state_dir, action),
        Commands::Topic { action } => topic::run(state_dir, action),
        Commands::Monitor { action } => monitor::run(state_dir, action),
        Commands::Status {
            group,
            all,
            json,
            watch,
            placement,
        } => status::run(
            state_dir,
            group,
            all,
            json,
            watch,
            placement.map(PlacementArg::placement),
        ),
        Commands::LocalAgent { .. } => Err(Error::usage(
            "local-agent coordinates a hosted org with this machine. Select that org, or switch back from `--org local`. Nothing was claimed.",
        )),
        Commands::Team { .. } => Err(Error::usage(
            "team requires a hosted org: pass --org <org> for a logged-in workspace, or switch back from `--org local`. Nothing was sent",
        )),
        Commands::McpPermission { timeout_secs } => mcp_permission::run(state_dir, timeout_secs),
        Commands::McpAgent => mcp_agent::run(state_dir),
        Commands::Artifact { action } => artifact::run(state_dir, action),
        Commands::Attachment { action } => attachment::run(state_dir, action),
        _ => unreachable!("agents dispatch received a non-agent command"),
    }
}
