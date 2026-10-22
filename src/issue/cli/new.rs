//! The new actions of the issue CLI.

use super::*;
use std::path::Path;

pub(crate) fn run(action: &IssueAction, state_dir: &Path) -> Result<i32> {
    match action {
        IssueAction::New {
            title,
            project,
            priority,
            parent,
            epic,
            tags,
            blocked_by,
            owner,
            component,
            id,
            file,
            status,
        } => {
            crate::master::refuse_in_app_conversation("issue new")?;
            let caller_is_master = crate::master::caller_is_master();
            write::master_issue_new_limits(
                caller_is_master,
                owner.as_deref(),
                id.as_deref(),
                status.as_deref(),
            )?;
            let body = match file {
                // The refusal is the master's confinement (CAD-614): its
                // body must be a file under master/tmp. Everyone else
                // may pipe it.
                Some(ref f) if f.as_os_str() == "-" && caller_is_master => {
                    return Err(Error::rejected(crate::master::NO_STDIN));
                }
                Some(ref f) if f.as_os_str() == "-" => Some(read_stdin_body("issue new --file")?),
                Some(ref f) => Some(crate::master::read_command_file(state_dir, f, u64::MAX).map_err(
                    |e| {
                        Error::rejected(format!(
                            "issue new --file {}: {e} — write it with the write tool into master/tmp, then --file",
                            f.display()
                        ))
                    },
                )?),
                None => None,
            };
            let pm = open_pm()?;
            let cwd = std::env::current_dir()?;
            let actor = if caller_is_master {
                crate::master::ALIAS
            } else {
                ""
            };
            let out = write::new_issue(
                &pm,
                &cwd,
                project.as_deref(),
                title,
                priority.as_deref(),
                parent.as_deref().or(epic.as_deref()),
                blocked_by,
                owner.as_deref(),
                component.as_deref(),
                tags,
                id.as_deref(),
                body.as_deref(),
                actor,
            )?;
            print_json(&out);
            Ok(0)
        }
        _ => unreachable!("new"),
    }
}
