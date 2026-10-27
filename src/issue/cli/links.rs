//! The links actions of the issue CLI.

use super::*;
use std::path::Path;

pub(crate) fn run(action: &IssueAction, state_dir: &Path) -> Result<i32> {
    match action {
        IssueAction::Link { id, kind, target } => {
            let pm = open_pm()?;
            let sd = Some(state_dir);
            print_json(&write::link(&pm, id, kind, target, false, None, "", sd)?);
            Ok(0)
        }
        IssueAction::Unlink { id, kind, target } => {
            let pm = open_pm()?;
            let sd = Some(state_dir);
            print_json(&write::link(&pm, id, kind, target, true, None, "", sd)?);
            Ok(0)
        }
        IssueAction::Ref {
            id,
            kind,
            target,
            label,
        } => {
            let pm = open_pm()?;
            print_json(&write::add_ref(
                &pm,
                id,
                kind,
                target,
                label.as_deref(),
                None,
                None,
                None,
                "",
            )?);
            Ok(0)
        }
        IssueAction::Comment {
            id,
            text,
            file,
            author,
            kind,
        } => {
            let body = match (text, file) {
                (Some(t), _) => t.clone(),
                (None, Some(f)) if f.as_os_str() == "-" => {
                    if crate::master::caller_is_master() {
                        return Err(Error::rejected(crate::master::NO_STDIN));
                    }
                    read_stdin_body("issue comment --file")?
                }
                (None, Some(f)) => std::fs::read_to_string(f)?,
                (None, None) => {
                    if crate::master::caller_is_master() {
                        return Err(Error::rejected(crate::master::NO_STDIN));
                    }
                    if atty_stdin() {
                        return Err(Error::rejected("Provide -m or --file"));
                    }
                    read_stdin_body("issue comment")?
                }
            };
            let pm = open_pm()?;
            print_json(&write::add_comment(
                &pm,
                id,
                &body,
                author.as_deref(),
                kind.as_deref(),
                None,
                "",
            )?);
            Ok(0)
        }
        IssueAction::Edit {
            id,
            set,
            tag,
            link,
            unlink,
            refs,
            unrefs,
            attach,
            acceptance,
            comment_file,
            author,
            force,
        } => {
            // The master has no stdin and no free file reads (CAD-614),
            // and `edit` is not on its allowlist; refuse here too so a
            // widened allowlist cannot open the stdin path.
            if crate::master::caller_is_master() {
                return Err(Error::rejected(
                    "issue edit is not available to the master — use the single-purpose verbs",
                ));
            }
            let is_stdin = |f: &Option<PathBuf>| f.as_ref().is_some_and(|p| p.as_os_str() == "-");
            if is_stdin(acceptance) && is_stdin(comment_file) {
                return Err(Error::rejected(
                    "edit: stdin can feed only one of --acceptance and --comment-file",
                ));
            }
            let read = |flag: &str, f: &Option<PathBuf>| -> Result<Option<String>> {
                match f {
                    None => Ok(None),
                    Some(p) if p.as_os_str() == "-" => Ok(Some(read_stdin_body(flag)?)),
                    Some(p) => std::fs::read_to_string(p).map(Some).map_err(|e| {
                        Error::rejected(format!("edit {flag}: cannot read {}: {e}", p.display()))
                    }),
                }
            };
            let spec = crate::issue::edit::EditSpec {
                set: set.clone(),
                tags: tag.clone(),
                link: link.clone(),
                unlink: unlink.clone(),
                refs: refs.clone(),
                unrefs: unrefs.clone(),
                attach: attach.clone(),
                acceptance: read("--acceptance", acceptance)?,
                comment: read("--comment-file", comment_file)?,
                author: author.clone(),
                force: force.clone(),
            };
            let pm = open_pm()?;
            let sd = Some(state_dir);
            let out = crate::issue::edit::edit(&pm, id, &spec, "", sd)?;
            print_json(&out);
            if out["worktree_open"].is_array() {
                eprintln!("{id}: worktree open: run cadence issue finish {id}");
            }
            Ok(0)
        }
        IssueAction::Attach { id, file } => {
            let pm = open_pm()?;
            print_json(&write::attach(&pm, id, file)?);
            Ok(0)
        }
        _ => unreachable!("links"),
    }
}
