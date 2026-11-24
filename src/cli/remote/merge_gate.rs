//! Hosted merge-gate, audit, delivery, and managed-caller refusal dispatch.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::env;
use std::fs;
use std::io;

use cadence_agent::delivery::hosted_gate::{self, GateBinding};
use cadence_agent::issue::cli::{IssueAction, ProjectAction};
use cadence_agent::remote_cli::{saved_capabilities, RemoteTarget};
use cadence_agent::{Error, Result};
use cadence_ops::audit as core_audit;
use serde_json::Value;

use super::scope_hint;
use crate::cli::audit::{
    full_head, identity_pairs, reviewer_subjects, AuditAction, IdentitiesAction, ReviewersAction,
};
use crate::cli::delivery::DeliveryAction;
use crate::cli::{print_json, Commands};

/// The explicit project a hosted gate call is scoped to. CAD-2001: the review
/// evidence is project-scoped, so a missing project is an error — it is never
/// inferred from `--repo` or the cwd checkout's origin, and no repo's first
/// project is ever selected.
fn project_arg(project: &Option<String>) -> Result<&str> {
    project.as_deref().ok_or_else(|| {
        Error::rejected(
            "a hosted `audit` call needs an explicit --project <key>: the review evidence is \
             project-scoped, and the project is never inferred from the repo or the cwd. \
             Nothing was sent",
        )
    })
}

/// The explicit issue a hosted approval is about. An approval is unique per
/// issue in its project, so an approval recorded for another ticket can never
/// answer this one.
fn issue_arg(issue: &Option<String>) -> Result<&str> {
    issue.as_deref().ok_or_else(|| {
        Error::rejected(
            "a hosted `audit approval|approve` needs an explicit --issue <ID>: the approval is \
             that issue's, and one recorded for another ticket never answers it. Nothing was \
             sent",
        )
    })
}

/// The exact `{project, repo, issue, pr, head}` binding every project-scoped
/// gate call carries. Each part is validated before a request exists.
fn binding(
    project: &Option<String>,
    repo: &str,
    issue: &str,
    pr: u64,
    head: &str,
) -> Result<GateBinding> {
    GateBinding::new(project_arg(project)?, repo, issue, pr, head)
}

/// CAD-1946: the merge gate's commands against a hosted org — the reads
/// `scripts/enqueue-reviewed` makes and the operator's merge approval.
/// `None` for every other command. Output and exit codes match the local
/// commands; the checks live in [`cadence_agent::delivery::hosted_gate`].
///
/// CAD-2001: every read and the approval are project-scoped. `--project` and
/// `--issue` are required here (a hosted call without them refuses rather
/// than guessing), and the tenant serves the project-scoped verbs. An older
/// tenant refuses those names, which
/// [`cadence_agent::remote_cli::PROJECT_SCOPED_GATE_VERBS`] reports as the
/// release-order problem it is — never as "no evidence".
pub(super) fn merge_gate(
    target: &RemoteTarget,
    command: &Commands,
    wake_timeout: u64,
) -> Result<Option<i32>> {
    let repo_or_origin = |repo: &Option<String>| -> Result<String> {
        match repo {
            Some(r) => Ok(r.clone()),
            None => core_audit::origin_slug(&env::current_dir()?).ok_or_else(|| {
                Error::rejected("no github.com origin remote in the cwd — pass --repo owner/name")
            }),
        }
    };
    let printed = |(doc, code): (Value, i32)| {
        print_json(&doc);
        Some(code)
    };
    Ok(match command {
        Commands::Delivery {
            action: DeliveryAction::Requirements {
                issue, pr, head, ..
            },
        } => printed((
            hosted_gate::requirements(target, wake_timeout, issue, pr, head)?,
            0,
        )),
        Commands::Delivery {
            action:
                DeliveryAction::Check {
                    project,
                    issue,
                    repo,
                    pr,
                    head,
                    browser_qa,
                },
        } => {
            let repo = repo_or_origin(repo)?;
            let head = head.as_deref().map(full_head).transpose()?;
            printed(hosted_gate::delivery_check(
                target,
                wake_timeout,
                project.as_deref(),
                issue.as_deref().map(str::trim),
                &repo,
                *pr,
                head.as_deref(),
                *browser_qa,
            )?)
        }
        Commands::Delivery {
            action:
                DeliveryAction::Enqueue {
                    project,
                    issue,
                    repo,
                    pr,
                    head,
                    browser_qa,
                    approve,
                },
        } => {
            refuse_managed(target, "delivery enqueue")?;
            let repo = repo_or_origin(repo)?;
            let head = full_head(head)?;
            let receipt = hosted_gate::delivery_enqueue(
                target,
                wake_timeout,
                project.as_deref(),
                issue.as_deref().map(str::trim),
                &repo,
                *pr,
                &head,
                *browser_qa,
                approve.as_deref(),
            )
            .map_err(|error| {
                scope_hint(
                    error,
                    &saved_capabilities(target),
                    "audit.approve",
                    "cadence login --audit-approve",
                )
            })?;
            printed((receipt, 0))
        }
        Commands::Audit {
            action:
                Some(AuditAction::Producer {
                    project,
                    repo,
                    issue,
                    pr,
                    head,
                    source,
                    human_only,
                    profile,
                }),
            ..
        } => {
            if source.is_some() {
                refuse_managed(target, "audit producer --source")?;
            }
            let binding = GateBinding::new(project, repo, issue, *pr, &full_head(head)?)?;
            let result = hosted_gate::review_producer(
                target,
                wake_timeout,
                &binding,
                source.as_deref(),
                *human_only,
                profile,
            )
            .map_err(|error| {
                if source.is_none() {
                    return error;
                }
                scope_hint(
                    error,
                    &saved_capabilities(target),
                    "audit.approve",
                    "cadence login --audit-approve",
                )
            })?;
            printed((result, 0))
        }
        Commands::Audit {
            action:
                Some(AuditAction::Approval {
                    pr,
                    head,
                    issue,
                    project,
                    repo,
                }),
            ..
        } => {
            let head = full_head(head)?;
            let repo = repo_or_origin(repo)?;
            printed(hosted_gate::approval(
                target,
                wake_timeout,
                &binding(project, &repo, issue_arg(issue)?, *pr, &head)?,
            )?)
        }
        Commands::Audit {
            action:
                Some(AuditAction::Verdict {
                    issue,
                    pr,
                    head,
                    kind,
                    outcome,
                    title_outcome,
                    section_outcome,
                    risk,
                    file,
                    text,
                    repo,
                    project,
                }),
            ..
        } => {
            let head = full_head(head)?;
            // AOS-473: the kinds the merge gate counts. `spec` is retired: the
            // service refuses it, and a hosted Spec verdict under a name the
            // gate never counts would refuse every two-review hosted PR.
            if !["standards", "spec-security", "combined", "browser-qa"].contains(&kind.as_str()) {
                return Err(Error::usage(
                    "audit verdict --kind is `standards`, `spec-security`, `combined` or \
                     `browser-qa`; nothing was sent",
                ));
            }
            if outcome != "pass" && outcome != "revise" {
                return Err(Error::usage(
                    "audit verdict --outcome is `pass` or `revise`; nothing was sent",
                ));
            }
            let title_outcome = title_outcome.as_deref().unwrap_or(outcome);
            let section_outcome = section_outcome.as_deref().unwrap_or(outcome);
            for value in [title_outcome, section_outcome] {
                if value != "pass" && value != "revise" {
                    return Err(Error::usage(
                        "audit verdict title/section outcome is `pass` or `revise`; nothing was sent",
                    ));
                }
            }
            // The findings are read client-side; `-` is stdin. A verdict
            // without findings is refused before send.
            let body =
                match (text, file) {
                    (Some(t), _) => t.clone(),
                    (None, Some(f)) if f.as_os_str() == "-" => io::read_to_string(io::stdin())?,
                    (None, Some(f)) => fs::read_to_string(f).map_err(|e| {
                        Error::rejected(format!(
                            "audit verdict --file {}: cannot read it locally: {e}",
                            f.display()
                        ))
                    })?,
                    (None, None) => return Err(Error::usage(
                        "a verdict needs its findings — pass --text or --file; nothing was sent",
                    )),
                };
            if body.trim().is_empty() {
                return Err(Error::usage(
                    "a verdict needs its findings — nothing was sent",
                ));
            }
            if body.len() > 16 * 1024 {
                return Err(Error::usage(
                    "verdict text exceeds the 16 KB cap — trim it; nothing was sent",
                ));
            }
            let repo = repo_or_origin(repo)?;
            let recorded = hosted_gate::verdict(
                target,
                wake_timeout,
                &binding(project, &repo, issue, *pr, &head)?,
                kind,
                outcome,
                title_outcome,
                section_outcome,
                risk,
                &body,
            )?;
            printed((recorded, 0))
        }
        Commands::Audit {
            action:
                Some(AuditAction::Verdicts {
                    issue,
                    pr,
                    head,
                    repo,
                    project,
                    body,
                    ..
                }),
            ..
        } => {
            let head = full_head(head)?;
            let repo = repo_or_origin(repo)?;
            printed(hosted_gate::verdicts(
                target,
                wake_timeout,
                &binding(project, &repo, issue, *pr, &head)?,
                *body,
            )?)
        }
        Commands::Audit {
            action: Some(AuditAction::Reviewers { project, action }),
            ..
        } => match action {
            None => {
                let project = project
                    .as_deref()
                    .ok_or_else(|| Error::rejected("audit reviewers needs --project"))?;
                printed(hosted_gate::reviewers(target, wake_timeout, project)?)
            }
            Some(ReviewersAction::Set { project, reviewer }) => {
                // The eligibility map is the workspace owner's own act: the
                // stored login is not an agent's to spend (the CAD-1586 rule).
                refuse_managed(target, "audit reviewers set")?;
                let subjects = reviewer_subjects(reviewer)?;
                printed((
                    hosted_gate::set_reviewers(target, wake_timeout, project, &subjects)?,
                    0,
                ))
            }
        },
        Commands::Audit {
            action: Some(AuditAction::Identities { action }),
            ..
        } => match action {
            None => printed(hosted_gate::identities(target, wake_timeout)?),
            Some(IdentitiesAction::Set { identity }) => {
                // Attesting a login decides whose verdicts count, so it is the
                // owner's own act exactly like the merge approval.
                refuse_managed(target, "audit identities set")?;
                let pairs = identity_pairs(identity)?;
                printed((
                    hosted_gate::set_identities(target, wake_timeout, &pairs)?,
                    0,
                ))
            }
        },
        Commands::Audit {
            action: Some(AuditAction::Scope { issue, project }),
            ..
        } => match project {
            Some(project) => printed(hosted_gate::scope_project(
                target,
                wake_timeout,
                project,
                issue.trim(),
            )?),
            None => printed(hosted_gate::scope(target, wake_timeout, issue.trim())?),
        },
        Commands::Audit {
            action:
                Some(AuditAction::Approve {
                    pr,
                    head,
                    issue,
                    delegated,
                    scope,
                    source,
                    repo,
                    action,
                    id,
                    project,
                }),
            ..
        } => {
            if action == "scope" {
                refuse_managed(target, "audit approve --action scope")?;
                if *delegated || scope.is_some() || pr.is_some() || head.is_some() {
                    return Err(Error::usage(
                        "remote `audit approve --action scope` names `--issue <ID> --project <KEY>` \
                         and never `--pr`, `--head`, `--delegated` or `--scope`. Nothing was sent.",
                    ));
                }
                let project = project_arg(project)?;
                let issue = issue_arg(issue)?.trim();
                let receipt = hosted_gate::approve_scope_project(
                    target,
                    wake_timeout,
                    project,
                    issue,
                    source,
                    id.as_deref(),
                )
                .map_err(|error| {
                    scope_hint(
                        error,
                        &saved_capabilities(target),
                        "audit.approve",
                        "cadence login --audit-approve",
                    )
                })?;
                printed((receipt, 0))
            } else if *delegated {
                // CAD-1946: designated agents record delegated approvals using their own
                // `cli.write` identity; never refuse managed callers or require `audit.approve`.
                if action != "merge" || id.is_some() {
                    return Err(Error::usage(
                        "remote `audit approve --delegated` approves a merge and does not take \
                         `--id` or non-`merge` `--action`. Nothing was sent.",
                    ));
                }
                let pr = pr.ok_or_else(|| Error::rejected("--pr is required"))?;
                let head = full_head(head.as_deref().unwrap_or_default())?;
                let repo = repo_or_origin(repo)?;
                let receipt = hosted_gate::approve_delegated_project(
                    target,
                    wake_timeout,
                    project.as_deref(),
                    issue.as_deref().map(str::trim),
                    &repo,
                    pr,
                    &head,
                    scope.as_deref(),
                    source,
                )?;
                printed((receipt, 0))
            } else {
                refuse_managed(target, "audit approve")?;
                if scope.is_some() || id.is_some() || action != "merge" {
                    return Err(Error::usage(
                        "remote `audit approve` records a merge approval (`--action merge` or \
                         `--action scope`) only; `--scope` requires `--delegated` and `--id` \
                         is only accepted for `--action scope`. Nothing was sent.",
                    ));
                }
                let pr = pr.ok_or_else(|| Error::rejected("--pr is required"))?;
                let head = full_head(head.as_deref().unwrap_or_default())?;
                let repo = repo_or_origin(repo)?;
                let receipt = hosted_gate::approve(
                    target,
                    wake_timeout,
                    &binding(project, &repo, issue_arg(issue)?, pr, &head)?,
                    source,
                )
                .map_err(|error| {
                    scope_hint(
                        error,
                        &saved_capabilities(target),
                        "audit.approve",
                        "cadence login --audit-approve",
                    )
                })?;
                printed((receipt, 0))
            }
        }
        Commands::Audit {
            action:
                Some(AuditAction::Designate {
                    alias,
                    project,
                    source,
                    revoke,
                }),
            ..
        } => {
            refuse_managed(target, "audit designate")?;
            let receipt = hosted_gate::designate_project(
                target,
                wake_timeout,
                alias,
                project,
                source,
                *revoke,
            )
            .map_err(|error| {
                scope_hint(
                    error,
                    &saved_capabilities(target),
                    "audit.approve",
                    "cadence login --audit-approve",
                )
            })?;
            printed((receipt, 0))
        }
        Commands::Audit {
            action: Some(AuditAction::Designations { project }),
            ..
        } => {
            let doc = hosted_gate::designations_project(target, wake_timeout, project.as_deref())?;
            printed((doc, 0))
        }
        Commands::Audit {
            action: Some(AuditAction::Digest { since, project }),
            ..
        } => {
            let doc = hosted_gate::digest_project(
                target,
                wake_timeout,
                project.as_deref(),
                since.as_deref(),
            )?;
            printed((doc, 0))
        }
        Commands::Audit {
            action:
                Some(AuditAction::Revoke {
                    id,
                    project,
                    source,
                    reason,
                }),
            ..
        } => {
            refuse_managed(target, "audit revoke")?;
            if source.is_some() {
                return Err(Error::rejected(
                    "`--source` is not supported on a hosted org; the authenticated owner session is recorded automatically.",
                ));
            }
            let receipt =
                hosted_gate::revoke_project(target, wake_timeout, project.as_deref(), id, reason)
                    .map_err(|error| {
                    scope_hint(
                        error,
                        &saved_capabilities(target),
                        "audit.approve",
                        "cadence login --audit-approve",
                    )
                })?;
            printed((receipt, 0))
        }
        Commands::Audit {
            action:
                Some(AuditAction::AcceptRevise {
                    pr,
                    head,
                    issue,
                    project,
                    source,
                    repo,
                }),
            ..
        } => {
            refuse_managed(target, "audit accept-revise")?;
            let head = full_head(head)?;
            let repo = repo_or_origin(repo)?;
            let project = Some(project.clone());
            let receipt = hosted_gate::accept_revise(
                target,
                wake_timeout,
                &binding(&project, &repo, issue.trim(), *pr, &head)?,
                source,
            )
            .map_err(|error| {
                scope_hint(
                    error,
                    &saved_capabilities(target),
                    "audit.approve",
                    "cadence login --audit-approve",
                )
            })?;
            printed((receipt, 0))
        }
        Commands::Audit {
            action:
                Some(AuditAction::Findings {
                    pr,
                    head,
                    issue,
                    project,
                    kind,
                    repo,
                }),
            ..
        } => {
            let head = full_head(head)?;
            let repo = repo_or_origin(repo)?;
            let project = Some(project.clone());
            let doc = hosted_gate::findings(
                target,
                wake_timeout,
                &binding(&project, &repo, issue.trim(), *pr, &head)?,
                kind.as_deref(),
            )?;
            printed((doc, 0))
        }
        Commands::Issue {
            action:
                IssueAction::Project {
                    action:
                        ProjectAction::Delivery {
                            key,
                            repos,
                            delivery_file,
                            show,
                            preview,
                            expect_rev,
                            preview_digest,
                        },
                },
        } => {
            if *show {
                return Ok(printed((
                    hosted_gate::show_delivery(target, wake_timeout, key)?,
                    0,
                )));
            }
            if !*preview {
                refuse_managed(target, "issue project delivery")?;
            }
            let delivery = delivery_file
                .as_ref()
                .map(|path| {
                    fs::read_to_string(path).map_err(|e| {
                        Error::rejected(format!("--delivery-file {}: {e}", path.display()))
                    })
                })
                .transpose()?;
            let receipt = if *preview {
                hosted_gate::preview_delivery(
                    target,
                    wake_timeout,
                    key,
                    repos,
                    delivery.as_deref(),
                )?
            } else {
                hosted_gate::set_delivery(
                    target,
                    wake_timeout,
                    key,
                    repos,
                    delivery.as_deref(),
                    expect_rev.zip(preview_digest.as_deref()),
                )?
            };
            printed((receipt, 0))
        }
        _ => None,
    })
}

/// The stored login is the human owner's; a managed caller spending it
/// would act as the owner (the CAD-1586 rule in [`super::run`]). It covers the merge
/// approval, the project eligibility map and the workspace identity
/// attestation: each decides whose verdicts count.
pub(super) fn refuse_managed(target: &RemoteTarget, command: &str) -> Result<()> {
    if target.identity.is_some()
        || env::var_os("CADENCE_ALIAS").is_some()
        || env::var_os("CADENCE_GRANT_TOKEN").is_some()
    {
        return Err(Error::rejected(format!(
            "remote `{command}` refused for a managed caller: it is the workspace \
             owner's own act and the stored login is not an agent's to spend. \
             Nothing was sent."
        )));
    }
    cadence_agent::remote_cli::refuse_non_human_actor(Some(target), command)?;
    Ok(())
}
