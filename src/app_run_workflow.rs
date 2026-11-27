//! Compile local app workflows into the frozen definitions consumed by Store.
//! Installed skill resolution stays with the caller; compilation does not access Store.

use std::collections::BTreeMap;

use cadence_core::connection_dispatch::exact_byte_digest;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::issue::{parse, plan, workflow};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalStep {
    pub id: String,
    pub kind: String,
    pub assignee: String,
    pub dependencies: Vec<String>,
    pub instruction: String,
    /// CAD-1867: the skill package this step was frozen with — resolved from
    /// the INSTALLED bundle's file map at run creation and stored inside the
    /// snapshot, so the digest covers it. `None` for a step that declares no
    /// `skill:`. Nothing is materialized on disk: these bytes travel in the
    /// kickoff envelope and nowhere else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill: Option<crate::issue::app::SkillPackage>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalWorkflow {
    pub source_digest: String,
    pub title: String,
    pub steps: Vec<LocalStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication_slot: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capability_slots: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_asset_slot: Option<String>,
    /// CAD-1171: `agent` (default, every step runs through its worker) or
    /// `host` (capability steps execute in-process for the operator's own
    /// click — no owner PM, no assignments).
    #[serde(default = "execution_agent")]
    pub execution: String,
}
fn execution_agent() -> String {
    "agent".into()
}

/// The non-installed resolver: a `skill:` tag names material only an installed
/// bundle can freeze, so it refuses rather than resolving to nothing.
fn refuse_skill_package(tag: &str) -> Result<crate::issue::app::SkillPackage> {
    Err(Error::rejected(format!(
        "`skill: {tag}` names installed-app skill material — only an installed app run \
         freezes a skill package into its snapshot"
    )))
}
impl LocalWorkflow {
    /// The NON-installed parse: no bundle is being frozen here, so a `skill:`
    /// directive refuses instead of being silently dropped.
    pub fn parse(text: &str, inputs: &BTreeMap<String, String>) -> Result<Self> {
        Self::parse_with(text, inputs, &refuse_skill_package)
    }

    /// CAD-1867: the installed parse. `resolve` maps a step's `skill: <tag>` to
    /// the package frozen out of the INSTALLED bundle's file map
    /// (`crate::issue::app::skill_package`), which is the same validator
    /// install ran — a package that is missing, incomplete or over bounds
    /// refuses here, before any run row exists.
    pub fn parse_installed(
        text: &str,
        inputs: &BTreeMap<String, String>,
        resolve: &dyn Fn(&str) -> Result<crate::issue::app::SkillPackage>,
    ) -> Result<Self> {
        Self::parse_with(text, inputs, resolve)
    }

    fn parse_with(
        text: &str,
        inputs: &BTreeMap<String, String>,
        resolve: &dyn Fn(&str) -> Result<crate::issue::app::SkillPackage>,
    ) -> Result<Self> {
        let template = workflow::parse_template(text)?;
        let publication_slot = template.publication_slot;
        let capability_slots = template.capability_slots;
        let required_asset_slot = template.required_asset_slot;
        let execution = template.execution.as_str().to_string();
        // CAD-1867: the per-step `skill:` tags of the TEMPLATE — the tags are
        // static (a placeholder refuses), so the render cannot change them.
        let skill_tags = workflow::skill_tags(text)?;
        let rendered = workflow::render(text, inputs)?;
        let parsed = plan::parse_plan(&rendered)?;
        let (_, body) =
            parse::split_front(&rendered).map_err(|e| Error::rejected(e.to_string()))?;
        let metadata = workflow::ticket_meta(body)?;
        if parsed.tickets.len() > 16 {
            return Err(Error::rejected("local runs support at most 16 steps"));
        }
        if skill_tags.len() != parsed.tickets.len() {
            return Err(Error::rejected(
                "the rendered workflow has a different step count than its template",
            ));
        }
        let mut steps = Vec::new();
        for (n, ticket) in parsed.tickets.iter().enumerate() {
            let meta: BTreeMap<_, _> = metadata[n].iter().cloned().collect();
            if meta.len() != metadata[n].len() {
                return Err(Error::rejected("duplicate local step metadata"));
            }
            for line in ticket
                .description
                .lines()
                .take_while(|line| !line.trim().is_empty())
            {
                if let Some((key, _)) = line.split_once(':') {
                    // CAD-1867: `skill:` is a metadata line the plan parser
                    // leaves in the body, so it appears here; the installed
                    // parse resolves it, and the non-installed one refuses it.
                    if !matches!(
                        key.trim(),
                        "action" | "uses" | "tries" | "reviewer" | "skill"
                    ) {
                        return Err(Error::rejected("unsupported local step directive"));
                    }
                }
            }

            if meta.contains_key("tries")
                || meta.contains_key("reviewer")
                || meta.contains_key("uses")
            {
                return Err(Error::rejected("local runs require explicit review steps and support one attempt; reviewer/tries directives are unsupported"));
            }
            let kind = match meta.get("action").map(String::as_str) {
                Some("local.text.produce") => "produce_text",
                Some("local.text.review") => "review_text",
                // CAD-1171: the host itself calls the frozen capability
                // slots; the step produces no text artifact.
                Some("local.capability.call") => "capability",
                _ => return Err(Error::rejected("unsupported execution capability: declare action: local.text.produce, local.text.review or local.capability.call")),
            };
            let assignee = match ticket.agent.clone() {
                Some(assignee) => {
                    crate::proto::identifier(&assignee, "local step agent")?;
                    assignee
                }
                // A capability step runs on the host; it names no worker.
                None if kind == "capability" => String::new(),
                None => {
                    return Err(Error::rejected(
                        "every local step needs an explicit assigned agent",
                    ))
                }
            };
            let dependencies = ticket
                .depends_on
                .iter()
                .map(|dep| match dep {
                    plan::Dep::Ticket(index) if *index < n => Ok(format!("s{}", index + 1)),
                    _ => Err(Error::rejected(
                        "local dependencies must refer to earlier steps of this run",
                    )),
                })
                .collect::<Result<Vec<_>>>()?;
            if kind == "review_text" {
                if dependencies.len() != 1 {
                    return Err(Error::rejected(
                        "review requires exactly one producer dependency",
                    ));
                }
                let producer: &LocalStep = &steps[ticket
                    .depends_on
                    .iter()
                    .find_map(|d| {
                        if let plan::Dep::Ticket(i) = d {
                            Some(*i)
                        } else {
                            None
                        }
                    })
                    .unwrap()];
                if producer.kind != "produce_text" || producer.assignee == assignee {
                    return Err(Error::rejected(
                        "reviewer must independently review a producer artifact",
                    ));
                }
            }
            if kind == "capability" && !assignee.is_empty() {
                return Err(Error::rejected("a capability step names no agent"));
            }
            if kind == "capability" && !dependencies.is_empty() {
                return Err(Error::rejected(
                    "a host capability step takes no dependencies",
                ));
            }
            let instruction = format!("{}\n{}", ticket.title, ticket.description);
            if instruction.len() > 16 * 1024 {
                return Err(Error::rejected(
                    "local step instruction exceeds its byte limit",
                ));
            }
            // CAD-1867: a declared tag is resolved NOW, from the installed
            // bundle, and frozen into the step. A missing or incomplete
            // package refuses here — before the run row exists.
            let skill = match skill_tags[n].clone() {
                Some(tag) => {
                    // A host capability step runs in-process with no worker
                    // and no kickoff, so frozen material would be dropped
                    // silently. Refuse rather than pretend.
                    if kind == "capability" {
                        return Err(Error::rejected(format!(
                            "step `skill: {tag}` — a host capability step runs no worker, \
                             so it cannot use skill material"
                        )));
                    }
                    Some(resolve(&tag).map_err(|error| {
                        Error::rejected(format!("step `skill: {tag}` — {error}"))
                    })?)
                }
                None => None,
            };
            steps.push(LocalStep {
                id: format!("s{}", n + 1),
                kind: kind.into(),
                assignee,
                dependencies,
                instruction,
                skill,
            });
        }
        if execution == "host" && steps.iter().any(|step| step.kind != "capability") {
            return Err(Error::rejected(
                "a host-execution workflow runs capability steps only",
            ));
        }
        if execution == "host" && steps.len() != 1 {
            return Err(Error::rejected(
                "a host-execution workflow declares exactly one step",
            ));
        }
        if execution != "host" && steps.iter().any(|step| step.kind == "capability") {
            return Err(Error::rejected(
                "a capability step requires execution: host",
            ));
        }
        Ok(Self {
            source_digest: exact_byte_digest(text.as_bytes()),
            title: parsed.title,
            steps,
            publication_slot,
            capability_slots,
            required_asset_slot,
            execution,
        })
    }

    pub fn validate_template(text: &str) -> Result<()> {
        let template = workflow::parse_template(text)?;
        // CAD-1171: a host-execution workflow's one step is the host's own
        // capability call; agent workflows keep the text actions.
        let host = template.execution == crate::issue::workflow::Execution::Host;
        let (_, body) = parse::split_front(text).map_err(|e| Error::rejected(e.to_string()))?;
        let metadata = workflow::ticket_meta(body)?;
        if metadata.is_empty() || metadata.len() > 16 {
            return Err(Error::rejected(
                "local workflow needs 1 to 16 explicitly supported steps",
            ));
        }
        for step in metadata {
            let fields: BTreeMap<_, _> = step.into_iter().collect();
            let supported = if host {
                matches!(
                    fields.get("action").map(String::as_str),
                    Some("local.capability.call")
                )
            } else {
                matches!(
                    fields.get("action").map(String::as_str),
                    Some("local.text.produce" | "local.text.review")
                )
            };
            if fields.contains_key("uses")
                || fields.contains_key("tries")
                || fields.contains_key("reviewer")
                || !supported
            {
                return Err(Error::rejected("local capability approval requires explicit supported action steps; uses, tries and implicit reviewer are unsupported"));
            }
        }
        Ok(())
    }
}
