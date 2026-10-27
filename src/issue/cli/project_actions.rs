//! The project actions of the issue CLI.

use super::*;
use std::path::Path;

pub(crate) fn run(action: &IssueAction, state_dir: &Path) -> Result<i32> {
    match action {
        IssueAction::Init => {
            let dir = crate::issue::default_dir()?;
            let pm = Pm::init(&dir)?;
            let installed = hooks::install(&pm.dir)?;
            print_json(&json!({"pm_dir": pm.dir,
                               "git": hooks::git_dir(&pm.dir).is_some(),
                               "hooks": installed}));
            Ok(0)
        }
        IssueAction::Doctor => {
            let pm = open_pm()?;
            let report = doctor::run(&pm)?;
            if report["ok"].as_bool() == Some(true) {
                print_json(&report);
                Ok(0)
            } else {
                eprintln!(
                    "{}",
                    serde_json::to_string_pretty(&report).unwrap_or_default()
                );
                Ok(1)
            }
        }
        IssueAction::Project { action } => match action {
            ProjectAction::Add {
                key,
                prefix,
                repos,
                components,
                tags,
                owner,
            } => {
                let pm = open_pm()?;
                let out = write::project_add(
                    &pm,
                    key,
                    prefix,
                    repos,
                    components,
                    tags,
                    owner.as_deref(),
                )?;
                print_json(&out);
                Ok(0)
            }
            ProjectAction::SetRemote { key, from, to } => {
                let pm = open_pm()?;
                let out = write::set_project_remote(&pm, key, from, to, "")?;
                print_json(&out);
                Ok(0)
            }
            ProjectAction::Policy {
                key,
                visibility,
                grants,
            } => {
                let pm = open_pm()?;
                let out = write::set_project_policy(&pm, key, visibility.recorded(), grants, "")?;
                print_json(&out);
                Ok(0)
            }
            ProjectAction::ApproveWork { key } => {
                model::check_key(key)?;
                print_json(&crate::client::rpc(
                    state_dir,
                    "project_work_approve",
                    json!({"project": key}),
                )?);
                Ok(0)
            }
            ProjectAction::Ls {
                sort,
                limit,
                fields,
                json: _,
            } => {
                let pm = open_pm()?;
                let projects = project::list(&pm.dir)?;
                // CAD-826: the resolved delivery policy per project. A
                // malformed section or an unreachable daemon degrades
                // that project's entry, never the listing.
                let approvals = crate::issue::delivery_policy::fetch_approvals(state_dir);
                let mut rows: Vec<Value> = projects
                    .iter()
                    .map(|p| {
                        let file = crate::issue::delivery_policy::load(&pm.dir, &p.key);
                        let eff = crate::issue::delivery_policy::effective(
                            &p.key,
                            file,
                            approvals.get(&p.key),
                        );
                        json!({
                            "key": p.key, "prefix": p.prefix,
                            "components": p.components,
                            "tags": p.tags,
                            "default_owner": p.default_owner,
                            "repos": p.repos.iter().map(|r| json!({
                                "path": r.path, "remote": r.remote,
                            })).collect::<Vec<_>>(),
                            "delivery": {
                                "source": eff.source,
                                "digest": eff.digest,
                                "note": eff.note,
                            },
                        })
                    })
                    .collect();
                if let Some(spec) = sort {
                    crate::filter::sort_rows(
                        &mut rows,
                        spec,
                        &[
                            ("key", "key"),
                            ("prefix", "prefix"),
                            ("default_owner", "default_owner"),
                        ],
                        "key",
                    )?;
                }
                crate::filter::apply_limit(&mut rows, *limit);
                crate::filter::apply_fields(&mut rows, fields)?;
                print_json(&json!({ "projects": rows }));
                Ok(0)
            }
        },
        _ => unreachable!("project"),
    }
}
