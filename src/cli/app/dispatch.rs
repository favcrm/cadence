//! Command dispatch and offline check/inspect runners for `cadence app`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::{Path, PathBuf};

use cadence_agent::client;
use cadence_agent::error::{Error, Result};
use cadence_agent::output::print_json;
use serde_json::json;

use super::params::{
    audience_params, binding_params, content_params, effect_params, record_params, works_params,
};
use super::runs::{
    assistant_scope, derive_operation_id, parse_assistant_input, read_run_inputs, run_params,
    run_selected_ticket, wait_assistant_operation, AssistantScope,
};
use super::{
    catalogue_params, install_source, run_dev, AccessAction, AppAction, AssistantAction,
    CatalogAction, ContextAction, RunAction,
};

/// `cadence app …` (CAD-547). `install`/`update`/`set`/`remove` write
/// the tracker directly — one commit each, `Actor:` recorded, all
/// unapproving-by-construction (a tracker write can only change the
/// digest, never the approval record) — `ls`/`show` read; `approve`
/// is the operator's daemon call, like `workflow approve`.
fn run_app(state_dir: &Path, action: AppAction) -> Result<i32> {
    use cadence_agent::issue::app;
    let result = match &action {
        AppAction::Describe { .. }
        | AppAction::Call { .. }
        | AppAction::Skill { .. }
        | AppAction::Pending { .. } => {
            return Err(Error::usage(
                "app describe/call/skill/pending require a hosted workspace; pass --org <workspace>",
            ));
        }
        AppAction::Binding { action } => {
            let (method, params) = binding_params(action);
            client::rpc(state_dir, method, params)?
        }
        AppAction::Effect { action } => {
            let (method, params) = effect_params(action);
            client::rpc(state_dir, method, params)?
        }
        AppAction::Context { action } => {
            let (method, params) = match action {
                ContextAction::Create {
                    install_id,
                    label,
                    defaults,
                    request_id,
                } => (
                    "app_context_create",
                    json!({"install_id":install_id,"label":label,"input_defaults":match defaults {Some(path)=>read_run_inputs(path)?,None=>json!({})},"request_id":request_id}),
                ),
                ContextAction::Ls { install_id } => {
                    ("app_context_list", json!({"install_id":install_id}))
                }
                ContextAction::Show {
                    install_id,
                    context_id,
                } => (
                    "app_context_show",
                    json!({"install_id":install_id,"context_id":context_id}),
                ),
                ContextAction::Set {
                    install_id,
                    context_id,
                    expected_revision,
                    label,
                    defaults,
                } => (
                    "app_context_update",
                    json!({"install_id":install_id,"context_id":context_id,"expected_revision":expected_revision,"label":label,"input_defaults":read_run_inputs(defaults)?}),
                ),
                ContextAction::Archive {
                    install_id,
                    context_id,
                    expected_revision,
                } => (
                    "app_context_archive",
                    json!({"install_id":install_id,"context_id":context_id,"expected_revision":expected_revision}),
                ),
            };
            client::rpc(state_dir, method, params)?
        }
        AppAction::Catalog { action } => {
            let (method, params) = match action {
                CatalogAction::ReleaseCheck { .. }
                | CatalogAction::ReleaseInstall { .. }
                | CatalogAction::ReleaseUpdate { .. }
                | CatalogAction::ReleaseRevoke { .. }
                | CatalogAction::Operation { .. }
                | CatalogAction::Pending { .. }
                | CatalogAction::Reconcile { .. } => {
                    return Err(Error::usage("published-release lifecycle commands require a selected remote workspace (--org); local app/catalog commands are unchanged"));
                }
                CatalogAction::Approve { install_id, digest } => (
                    "app_local_install_approve",
                    json!({"install_id":install_id,"digest":digest}),
                ),
                CatalogAction::Revoke { install_id, digest } => (
                    "app_local_install_revoke",
                    json!({"install_id":install_id,"digest":digest}),
                ),
                CatalogAction::Install {
                    source,
                    expected_digest,
                    catalogue_id,
                    catalogue_version,
                    catalogue_digest,
                    catalogue_resolution,
                } => {
                    let source = install_source(source)?;
                    let mut params = json!({"source":source});
                    if let Some(digest) = expected_digest {
                        params["expected_digest"] = json!(digest);
                    }
                    catalogue_params(
                        &mut params,
                        catalogue_id,
                        catalogue_version,
                        catalogue_digest,
                        catalogue_resolution,
                    )?;
                    ("app_workspace_install", params)
                }
                CatalogAction::InstallCheck {
                    source,
                    catalogue_id,
                    catalogue_version,
                    catalogue_digest,
                    catalogue_resolution,
                } => {
                    let source = install_source(source)?;
                    let mut params = json!({"source":source});
                    catalogue_params(
                        &mut params,
                        catalogue_id,
                        catalogue_version,
                        catalogue_digest,
                        catalogue_resolution,
                    )?;
                    ("app_workspace_install_check", params)
                }
                CatalogAction::Upgrade {
                    install_id,
                    source,
                    expected_digest,
                    expected_generation,
                    expected_new_digest,
                    request_id,
                    catalogue_id,
                    catalogue_version,
                    catalogue_digest,
                    catalogue_resolution,
                } => {
                    let source = if source.contains("://")
                        || source.starts_with("git@")
                        || source.starts_with("builtin:")
                    {
                        source.clone()
                    } else {
                        // A committed request is replayable after its local
                        // checkout is removed. The daemon verifies the exact
                        // stored source and digest before returning that row.
                        match std::fs::canonicalize(source) {
                            Ok(path) => path,
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                                std::path::absolute(source).map_err(|e| {
                                    Error::rejected(format!("workspace app source: {e}"))
                                })?
                            }
                            Err(error) => {
                                return Err(Error::rejected(format!(
                                    "workspace app source: {error}"
                                )))
                            }
                        }
                        .to_string_lossy()
                        .into_owned()
                    };
                    let mut params = json!({"install_id":install_id,"source":source,
                        "expected_digest":expected_digest,"expected_generation":expected_generation,
                        "expected_new_digest":expected_new_digest,"request_id":request_id});
                    catalogue_params(
                        &mut params,
                        catalogue_id,
                        catalogue_version,
                        catalogue_digest,
                        catalogue_resolution,
                    )?;
                    ("app_workspace_upgrade", params)
                }
                CatalogAction::UpgradeCheck {
                    install_id,
                    source,
                    expected_digest,
                    expected_generation,
                    catalogue_id,
                    catalogue_version,
                    catalogue_digest,
                    catalogue_resolution,
                } => {
                    let source = install_source(source)?;
                    let mut params = json!({"install_id":install_id,"source":source,
                        "expected_digest":expected_digest,"expected_generation":expected_generation});
                    catalogue_params(
                        &mut params,
                        catalogue_id,
                        catalogue_version,
                        catalogue_digest,
                        catalogue_resolution,
                    )?;
                    ("app_workspace_upgrade_check", params)
                }
                CatalogAction::UpgradeRecover {
                    install_id,
                    request_id,
                } => (
                    "app_workspace_upgrade_recover",
                    json!({"install_id":install_id,"request_id":request_id}),
                ),
                CatalogAction::MigrationRecover {
                    journal_id,
                    rollback,
                } => (
                    "app_workspace_migration_recover",
                    json!({"journal_id":journal_id,"rollback":rollback}),
                ),
                CatalogAction::Ls => ("app_workspace_list", json!({})),
                CatalogAction::Show { install_id } => {
                    ("app_workspace_show", json!({"install_id":install_id}))
                }
                CatalogAction::Migrate => ("app_workspace_migrate", json!({})),
                CatalogAction::Recover { install_id } => {
                    ("app_workspace_recover", json!({"install_id":install_id}))
                }
            };
            client::rpc(state_dir, method, params)?
        }
        AppAction::Run { action } => match action {
            RunAction::Ticket { .. } => run_selected_ticket(state_dir, action)?,
            _ => {
                let (method, params) = run_params(action)?;
                client::rpc(state_dir, method, params)?
            }
        },
        AppAction::Record { action } => {
            let (method, params) = record_params(action)?;
            client::rpc(state_dir, method, params)?
        }
        AppAction::Audience { action } => {
            let (method, params) = audience_params(action)?;
            client::rpc(state_dir, method, params)?
        }
        AppAction::Content { action } => {
            let (method, params) = content_params(action)?;
            client::rpc(state_dir, method, params)?
        }
        AppAction::Works { action } => {
            let (method, params) = works_params(action)?;
            client::rpc(state_dir, method, params)?
        }
        AppAction::Assistant { action } => {
            let (method, params, wait): (
                &'static str,
                serde_json::Value,
                Option<(AssistantScope, String)>,
            ) = match action {
                AssistantAction::Actions {
                    install_id,
                    context_id,
                    message,
                    token,
                } => {
                    let scope = assistant_scope(
                        state_dir,
                        install_id.as_deref(),
                        context_id.as_deref(),
                        message.as_deref(),
                        token.as_deref(),
                    )?;
                    ("app_assistant_actions", scope.params(json!({})), None)
                }
                AssistantAction::Invoke {
                    install_id,
                    context_id,
                    message,
                    token,
                    action_id,
                    operation_id,
                    input,
                    wait,
                } => {
                    // The ticket's refusal order: malformed or oversized
                    // input never reaches the daemon, not even the read that
                    // resolves the live turn.
                    let input = parse_assistant_input(state_dir, input)?;
                    let scope = assistant_scope(
                        state_dir,
                        install_id.as_deref(),
                        context_id.as_deref(),
                        message.as_deref(),
                        token.as_deref(),
                    )?;
                    let operation_id = match operation_id {
                        Some(id) => id.clone(),
                        None => derive_operation_id(&scope.message, action_id, &input),
                    };
                    let params = scope.params(json!({
                        "action_id": action_id,
                        "operation_id": operation_id,
                        "input": input,
                    }));
                    let waiting = wait.then(|| (scope.clone(), operation_id));
                    ("app_assistant_invoke", params, waiting)
                }
                AssistantAction::OperationShow {
                    install_id,
                    context_id,
                    message,
                    token,
                    operation_id,
                } => {
                    let scope = assistant_scope(
                        state_dir,
                        install_id.as_deref(),
                        context_id.as_deref(),
                        message.as_deref(),
                        token.as_deref(),
                    )?;
                    (
                        "app_assistant_operation_show",
                        scope.params(json!({"operation_id":operation_id})),
                        None,
                    )
                }
                AssistantAction::Decide {
                    install_id,
                    context_id,
                    operation_id,
                    expected_revision,
                    decision,
                } => (
                    "app_assistant_decision",
                    json!({
                        "install_id": install_id,
                        "context_id": context_id,
                        "operation_id": operation_id,
                        "expected_revision": expected_revision,
                        "decision": decision,
                    }),
                    None,
                ),
            };
            let result = client::rpc(state_dir, method, params)?;
            match wait {
                Some((scope, operation_id)) => {
                    wait_assistant_operation(state_dir, &scope, &operation_id, result)?
                }
                None => result,
            }
        }
        AppAction::FrameKit {
            out,
            bundle,
            parent_origin,
            integrity_only,
        } => {
            return cadence_agent::app_preview_export::write_frame_kit(
                out,
                bundle.as_deref(),
                parent_origin,
                *integrity_only,
            );
        }
        AppAction::Fixtures {
            bundle,
            json,
            integrity_only,
        } => {
            return cadence_agent::app_preview_export::write_fixtures(
                bundle,
                *json,
                *integrity_only,
            );
        }
        AppAction::Dev {
            name,
            source,
            package,
            port,
            host,
            allow_host,
        } => {
            return run_dev(
                name.as_deref(),
                source.as_deref(),
                package.as_deref(),
                *port,
                host,
                allow_host,
            );
        }
        // Offline package commands dispatch before org/state-dir resolution.
        AppAction::Check { .. } | AppAction::Inspect { .. } => {
            unreachable!("offline app validation dispatches before state-dir resolution")
        }
        AppAction::Install { source, project } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::install(&pm, project, source, state_dir, "")?
        }
        AppAction::Ls { project } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::ls(&pm, project.as_deref(), state_dir)?
        }
        AppAction::Show { name, project } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::show(&pm, project, name, state_dir)?
        }
        AppAction::Set {
            name,
            bindings,
            project,
        } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::set(&pm, project, name, bindings, state_dir, "")?
        }
        AppAction::Update {
            name,
            source,
            project,
        } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::update(&pm, project, name, source.as_deref(), state_dir, "")?
        }
        AppAction::Remove { name, project } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::remove(&pm, project, name, state_dir, "")?
        }
        AppAction::Approve { name, project } => client::rpc(
            state_dir,
            "app_approve",
            json!({"project": project, "name": name}),
        )?,
        AppAction::Revoke { name, project } => client::rpc(
            state_dir,
            "app_revoke",
            json!({"project": project, "name": name}),
        )?,
        AppAction::SetTeam {
            name,
            roles,
            project,
        } => client::rpc(
            state_dir,
            "app_set_team",
            json!({"project": project, "name": name, "team": roles}),
        )?,
        AppAction::AddWorker {
            name,
            role,
            project,
        } => client::rpc(
            state_dir,
            "app_add_worker",
            json!({"project": project, "name": name, "role": role}),
        )?,
        AppAction::Access { action } => match action {
            AccessAction::Ls { install_id } => client::rpc(
                state_dir,
                "app_member_access_list",
                json!({"install_id": install_id}),
            )?,
            AccessAction::Revoke {
                install_id,
                member,
                conversation,
            } => {
                let mut body = json!({"install_id": install_id, "member_sub": member});
                if let Some(conversation) = conversation {
                    body["conversation_id"] = json!(conversation);
                }
                client::rpc(state_dir, "app_member_access_revoke", body)?
            }
        },
    };
    print_json(&result);
    Ok(0)
}

pub(in crate::cli) fn run(state_dir: PathBuf, action: AppAction) -> Result<i32> {
    run_app(&state_dir, action)
}

/// CAD-1270 `cadence app check <dir>`: the offline bundle validator.
/// Runs before org/state-dir resolution — it opens no state dir, daemon
/// socket or PM; the only filesystem it reads is the named directory. A
/// `builtin:` source gets the retired refusal (CAD-1422). The JSON
/// report prints to stdout; the exit code is the verdict (0 pass, 3
/// refusal — `rejected` in the kind table).
pub(in crate::cli) fn run_check(source: &str, _json: bool) -> Result<i32> {
    print_package_report(cadence_agent::issue::app_catalog::workspace::check_offline(
        source,
    )?)
}

pub(in crate::cli) fn run_inspect(source: &str, _json: bool) -> Result<i32> {
    print_package_report(cadence_agent::issue::app_catalog::workspace::inspect_offline(source)?)
}

fn print_package_report(report: serde_json::Value) -> Result<i32> {
    let ok = report["ok"].as_bool() == Some(true);
    // The report is written before the verdict is known, so it must not
    // be able to change it: `print_json` drops a closed consumer's EPIPE
    // instead of panicking, and this return still decides the status.
    print_json(&report);
    Ok(if ok { 0 } else { 3 })
}
