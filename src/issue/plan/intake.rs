//! Versioned source and scope evidence for ordinary goal-intake plans.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{PlanDoc, Ticket};
use crate::delegation::sha256_hex;
use crate::error::{Error, Result};
use crate::issue::{areas, context, model::Front, parse, project::Project};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GoalIntake {
    pub(crate) request_digest: String,
    pub(crate) revision: String,
    pub(crate) constraints: Vec<String>,
    pub(crate) success_criteria: Vec<String>,
    pub(crate) unknowns: Vec<String>,
    pub(crate) revises: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) superseded_by: Option<String>,
    pub(crate) goal_digest: String,
    sources: BTreeMap<String, Source>,
    pub(crate) outcomes: BTreeMap<String, Outcome>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct Source {
    path: String,
    digest: String,
    required: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct Outcome {
    pub(crate) reused: bool,
    sources: Vec<String>,
    paths: Vec<String>,
    acceptance: Vec<String>,
    scope_digest: String,
}

fn stale(detail: impl std::fmt::Display) -> Error {
    Error::invalid(
        "plan_evidence_stale",
        format!("{detail}; propose an intake revision and have the operator approve it"),
    )
}

fn snapshot(project: &Project, cited: impl Fn(&str) -> bool) -> Result<context::PinnedContext> {
    let pinned = context::pin_for_plan(project, cited);
    if pinned.state != "ready" || pinned.revision.is_none() {
        let detail = pinned.errors.join("; ");
        return Err(Error::invalid(
            "plan_context_unavailable",
            format!(
                "{} project context is {}: {detail}",
                project.key, pinned.state
            ),
        ));
    }
    Ok(pinned)
}

fn source(document: &context::PinnedDocument) -> Option<Source> {
    Some(Source {
        path: document.path.clone(),
        digest: document.digest.as_ref()?.clone(),
        required: document.required,
    })
}

impl GoalIntake {
    pub(crate) fn prepare(project: &Project, doc: &PlanDoc, goal: &str) -> Result<Option<Self>> {
        let Some(input) = &doc.intake else {
            return Ok(None);
        };
        let request_digest = doc
            .request_digest
            .as_ref()
            .ok_or_else(|| Error::internal("goal intake has no request digest"))?;
        let cited: BTreeSet<&str> = doc
            .tickets
            .iter()
            .flat_map(|ticket| ticket.sources.iter().map(String::as_str))
            .collect();
        let pinned = snapshot(project, |id| cited.contains(id))?;
        let revision = pinned.revision.as_deref().unwrap_or_default();
        let mut sources = BTreeMap::new();
        for id in pinned
            .documents
            .iter()
            .filter(|d| d.required)
            .map(|d| &d.id)
            .chain(doc.tickets.iter().flat_map(|t| &t.sources))
        {
            if sources.contains_key(id) {
                continue;
            }
            let document = pinned
                .documents
                .iter()
                .find(|d| &d.id == id)
                .ok_or_else(|| {
                    Error::invalid(
                        "plan_source_missing",
                        format!(
                            "source '{id}' is not registered for project {}",
                            project.key
                        ),
                    )
                })?;
            let evidence = source(document).ok_or_else(|| {
                Error::invalid(
                    "plan_source_missing",
                    format!("source '{id}' ({}): {}", document.path, document.state),
                )
            })?;
            sources.insert(id.clone(), evidence);
        }
        let paths: std::collections::BTreeSet<&str> = doc
            .tickets
            .iter()
            .flat_map(|t| t.paths.iter().map(String::as_str))
            .collect();
        context::check_plan_paths(project, revision, paths).map_err(|(state, detail)| {
            Error::invalid("plan_module_unavailable", format!("{state}: {detail}"))
        })?;
        Ok(Some(Self {
            request_digest: request_digest.clone(),
            revision: revision.to_string(),
            constraints: input.constraints.clone(),
            success_criteria: input.success_criteria.clone(),
            unknowns: input.unknowns.clone(),
            revises: input.revises.clone(),
            superseded_by: None,
            goal_digest: sha256_hex(goal.trim().as_bytes()),
            sources,
            outcomes: BTreeMap::new(),
        }))
    }

    pub(crate) fn record(&mut self, front: &Front, body: &str, ticket: &Ticket) -> Result<()> {
        if ticket.reuse.is_some() {
            let acceptance = parse::acceptance_items(body);
            let scope_matches = ticket.paths.iter().all(|path| front.paths.contains(path))
                && ticket
                    .acceptance
                    .iter()
                    .all(|item| acceptance.iter().any(|existing| existing.text == item.text));
            if !scope_matches {
                return Err(Error::invalid(
                    "plan_reuse_scope",
                    format!(
                        "reused issue {} does not already declare the mapped paths and acceptance",
                        front.id
                    ),
                ));
            }
        }
        self.outcomes.insert(
            front.id.clone(),
            Outcome {
                reused: ticket.reuse.is_some(),
                sources: ticket.sources.clone(),
                paths: ticket.paths.clone(),
                acceptance: ticket
                    .acceptance
                    .iter()
                    .map(|item| item.text.clone())
                    .collect(),
                scope_digest: scope_digest(front, body)?,
            },
        );
        Ok(())
    }

    pub(crate) fn require_goal(&self, body: &str) -> Result<()> {
        if self.goal_digest != sha256_hex(body.trim().as_bytes()) {
            return Err(stale("the plan goal or constraints changed"));
        }
        if !self.unknowns.is_empty() {
            return Err(Error::invalid(
                "plan_unknowns_unresolved",
                format!(
                    "resolve the plan's consequential unknowns: {}",
                    self.unknowns.join("; ")
                ),
            ));
        }
        Ok(())
    }

    pub(crate) fn require_scope(&self, front: &Front, body: &str) -> Result<()> {
        let outcome = self
            .outcomes
            .get(&front.id)
            .ok_or_else(|| stale(format!("{} has no recorded outcome", front.id)))?;
        if front.status == "dropped" {
            return Err(stale(format!("{} was dropped, not completed", front.id)));
        }
        if outcome.scope_digest != scope_digest(front, body)? {
            return Err(stale(format!("{} scope or acceptance changed", front.id)));
        }
        Ok(())
    }

    /// Required context applies to the entire plan; optional documents only
    /// invalidate the outcomes that cite them. Unrelated commits are safe.
    pub(crate) fn require_sources(&self, project: &Project, issue: Option<&Front>) -> Result<()> {
        let affected = || {
            self.outcomes.iter().filter(move |(id, _)| {
                issue.is_none_or(|front| &front.id == *id || front.blocked_by.contains(id))
            })
        };
        let cited: BTreeSet<&str> = affected()
            .flat_map(|(_, outcome)| outcome.sources.iter().map(String::as_str))
            .collect();
        let pinned = snapshot(project, |id| cited.contains(id)).map_err(stale)?;
        let revision = pinned.revision.as_deref().unwrap_or_default();
        for doc in &pinned.documents {
            if doc.required && self.sources.get(&doc.id).is_none_or(|src| !src.required) {
                return Err(stale(format!("new required context source '{}'", doc.id)));
            }
        }
        for (id, expected) in &self.sources {
            if !expected.required && !cited.contains(id.as_str()) {
                continue;
            }
            let matches = pinned.documents.iter().any(|doc| {
                &doc.id == id
                    && doc.path == expected.path
                    && doc.digest.as_deref() == Some(expected.digest.as_str())
                    && doc.required == expected.required
            });
            if !matches {
                return Err(stale(format!(
                    "context source '{id}' changed or is unreadable"
                )));
            }
        }
        let paths: std::collections::BTreeSet<&str> = affected()
            .flat_map(|(_, outcome)| outcome.paths.iter().map(String::as_str))
            .collect();
        context::check_plan_paths(project, revision, paths).map_err(|(_, detail)| stale(detail))
    }
}

fn scope_digest(front: &Front, body: &str) -> Result<String> {
    // A claim, assignee or status change is operational, not new work.
    for path in &front.paths {
        areas::check_path(path)?;
    }
    // Checking off existing criteria does not amend the approved scope.
    let mut acceptance = parse::acceptance_items(body);
    let body = if acceptance.is_empty() {
        Cow::Borrowed(body)
    } else {
        for item in &mut acceptance {
            item.checked = false;
        }
        Cow::Owned(parse::replace_acceptance(body, &acceptance)?)
    };
    let scope = serde_json::to_vec(&(&front.title, &front.paths, &front.blocked_by, body.trim()))
        .map_err(|err| Error::internal(format!("could not encode issue scope: {err}")))?;
    Ok(sha256_hex(&scope))
}
