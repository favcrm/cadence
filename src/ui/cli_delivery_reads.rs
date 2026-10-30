//! CAD-1946: the merge gate's three hosted reads — `delivery_requirements`,
//! `audit_approval` and `audit_verdicts` on `POST /api/cli/<verb>`.
//!
//! `scripts/enqueue-reviewed` decides a merge from these three answers.
//! On a hosted org there is no local daemon, so each one is answered here,
//! by this org's own sources, with the same code the local CLI runs:
//!
//! - requirements: the daemon's `delivery_requirements` RPC, i.e.
//!   `delivery_requirements_rpc::classify` (binds issue, project remotes,
//!   repo, base and live head, and re-reads for drift);
//! - approval: `audit::approval_check` on this org's store, for the
//!   repo/PR/head that classify just bound to the live PR;
//! - verdicts: `audit::verdicts_check` on this org's configured notes dir.
//!
//! Rules: `cli.read` scope (the route checks it), a human Owner only (the
//! `project_policy` rule: `role_at_issue == owner` and `principal_kind ==
//! user`), the issue's project must admit the caller, the arguments are a
//! closed set, and the caller never names a path, a policy or evidence.
//! Nothing here writes. Every failure is a refusal, never a pass.

use std::path::Path;

use serde::Deserialize;
use serde_json::{json, Value};

use super::{cli_fail, json_ok, pm_at, restricted, rpc, rpc_error, HttpResp};
use crate::cli_actor::{CliActor, Verb};
use crate::client;
use crate::issue::board;

#[cfg(test)]
#[path = "cad1946_delivery_reads_refusal.rs"]
mod cad1946_delivery_reads_refusal;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequirementsArgs {
    issue: String,
    /// The full GitHub PR URL, as `cadence delivery requirements --pr`.
    pr: String,
    head: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalArgs {
    issue: String,
    repo: String,
    pr: u64,
    head: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerdictsArgs {
    issue: String,
    pr: u64,
    head: String,
}

fn parse<T: serde::de::DeserializeOwned>(verb: &str, args: &Value) -> Result<T, HttpResp> {
    serde_json::from_value(args.clone())
        .map_err(|e| cli_fail(400, "invalid_request", &format!("{verb}: {e}")))
}

/// A pinned head is the full lower-case 40-hex SHA; anything else could
/// bind to another revision.
fn full_head(head: &str) -> Result<&str, HttpResp> {
    if head.len() == 40 && head.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        Ok(head)
    } else {
        Err(cli_fail(
            400,
            "invalid_request",
            "head must be the full lower-case 40-hex SHA",
        ))
    }
}

/// `owner/name` only — the value is spliced into a PR URL.
fn repo_slug(repo: &str) -> Result<String, HttpResp> {
    let ok_part = |p: &str| {
        !p.is_empty()
            && p != "."
            && p != ".."
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    };
    match repo.split_once('/') {
        Some((owner, name)) if ok_part(owner) && ok_part(name) => Ok(repo.to_ascii_lowercase()),
        _ => Err(cli_fail(400, "invalid_request", "repo must be owner/name")),
    }
}

/// The issue must exist and its project must admit the caller; a
/// restricted project answers exactly as an unknown issue does.
fn admit_issue(pm_dir: &Path, raw: &str, actor: &CliActor) -> Result<String, HttpResp> {
    let id = crate::issue::model::check_id(raw)
        .map_err(|_| cli_fail(400, "invalid_request", "bad issue id"))?;
    let pm = pm_at(pm_dir)?;
    let issue =
        board::find_issue(&pm.dir, &id).map_err(|_| cli_fail(404, "not_found", "unknown issue"))?;
    if let Some(refusal) = restricted(&pm, &issue.project, actor) {
        return Err(refusal);
    }
    Ok(id)
}

/// The three reads, after the route verified the envelope, its `cli.read`
/// scope and its single use.
pub(super) fn serve(
    verb: Verb,
    args: &Value,
    actor: &CliActor,
    state_dir: &Path,
    pm_dir: &Path,
) -> HttpResp {
    if actor.role_at_issue != "owner" || actor.principal_kind != "user" {
        return cli_fail(
            403,
            "not_permitted",
            "the merge gate reads are human-owner only — a member or an agent cannot read them",
        );
    }
    match answer(verb, args, actor, state_dir, pm_dir) {
        Ok(resp) | Err(resp) => resp,
    }
}

fn answer(
    verb: Verb,
    args: &Value,
    actor: &CliActor,
    state_dir: &Path,
    pm_dir: &Path,
) -> Result<HttpResp, HttpResp> {
    match verb {
        Verb::DeliveryRequirements => {
            let a: RequirementsArgs = parse("delivery_requirements", args)?;
            let head = full_head(&a.head)?;
            let issue = admit_issue(pm_dir, &a.issue, actor)?;
            Ok(rpc(
                state_dir,
                "delivery_requirements",
                json!({"issue": issue, "pr": a.pr, "head": head}),
            ))
        }
        Verb::AuditApproval => {
            let a: ApprovalArgs = parse("audit_approval", args)?;
            let head = full_head(&a.head)?;
            let repo = repo_slug(&a.repo)?;
            if a.pr == 0 {
                return Err(cli_fail(400, "invalid_request", "pr must be positive"));
            }
            let issue = admit_issue(pm_dir, &a.issue, actor)?;
            // Bind the read to the live PR first: classify refuses a moved
            // head, a closed PR, a title for another issue and a repo that
            // is not one of the ticket project's remotes. Only then is the
            // store asked about exactly that repo, PR and head.
            let bound = client::rpc(
                state_dir,
                "delivery_requirements",
                json!({"issue": issue, "pr": format!("https://github.com/{repo}/pull/{}", a.pr),
                       "head": head}),
            )
            .map_err(rpc_error)?;
            if bound["repo"].as_str() != Some(repo.as_str())
                || bound["pr"].as_u64() != Some(a.pr)
                || bound["head"].as_str() != Some(head)
            {
                return Err(cli_fail(
                    409,
                    "binding_mismatch",
                    "the live PR binding differs from the requested repo, PR or head",
                ));
            }
            let (state, _) = crate::audit::approval_check(state_dir, &repo, a.pr, head);
            Ok(json_ok(state))
        }
        Verb::AuditVerdicts => {
            let a: VerdictsArgs = parse("audit_verdicts", args)?;
            let head = full_head(&a.head)?;
            if a.pr == 0 {
                return Err(cli_fail(400, "invalid_request", "pr must be positive"));
            }
            let issue = admit_issue(pm_dir, &a.issue, actor)?;
            // The org's configured notes dir — never a caller-named path.
            let notes = pm_at(pm_dir)?.config.notes_dir();
            crate::audit::verdicts_check(Some(&notes), &issue, a.pr, head)
                .map(json_ok)
                .map_err(|e| cli_fail(503, "capability_unavailable", &e.to_string()))
        }
        _ => Err(cli_fail(404, "not_found", "no such cli route")),
    }
}
