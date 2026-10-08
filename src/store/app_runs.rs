//! Project-free, broker-local text runs. This is not provider tool confinement.
use super::StoreConn;
use super::*;
use crate::issue::{parse, plan, workflow};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const ARTIFACT_BYTES: usize = 256 * 1024;
pub const RUN_ARTIFACT_BYTES: usize = 1024 * 1024;

/// CAD-1123 R4: the stream that keeps each app run's approval record.
/// Not an agent alias and not the pruned daemon stream, so it survives.
pub const APP_RUN_APPROVAL_STREAM: &str = "app-run-approvals";
pub fn artifact_digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
/// CAD-1143 Redo-image byte rule: a retained caption must equal the frozen
/// carry bytes exactly — a worker echo is verified by digest, never trusted.
/// Runs without a text carry are unaffected.
fn carry_text_bytes_ok(run: &Value, text: &str) -> bool {
    run["snapshot"]["carry"]["retain"]
        .as_str()
        .is_none_or(|retain| {
            retain != "text"
                || artifact_digest(text.as_bytes())
                    == run["snapshot"]["carry"]["artifact_digest"]
                        .as_str()
                        .unwrap_or_default()
        })
}
pub fn material_digest(value: &Value) -> String {
    // serde_json's default Map is ordered; rebuilding explicitly also fixes
    // behavior if preserve_order is enabled by another dependency later.
    fn canonical(value: &Value) -> Value {
        match value {
            Value::Object(fields) => {
                let ordered: BTreeMap<_, _> = fields
                    .iter()
                    .map(|(k, v)| (k.clone(), canonical(v)))
                    .collect();
                Value::Object(ordered.into_iter().collect())
            }
            Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
            other => other.clone(),
        }
    }
    artifact_digest(format!("cadence-app-material-v1\n{}", canonical(value)).as_bytes())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalStep {
    pub id: String,
    pub kind: String,
    pub assignee: String,
    pub dependencies: Vec<String>,
    pub instruction: String,
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
    /// Redo carry halves this workflow may retain, from its `carries:`
    /// declaration. Constrained against the run request at start; never a
    /// step-skip control. Serialized into the frozen snapshot like the
    /// other declarations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carries: Vec<String>,
}
fn execution_agent() -> String {
    "agent".into()
}
impl LocalWorkflow {
    pub fn parse(text: &str, inputs: &BTreeMap<String, String>) -> Result<Self> {
        Self::parse_carry(text, inputs, &BTreeMap::new())
    }

    /// Parse with a run-owned carry map. CAD-1143 Redo: the retained
    /// caption is digest-verified material, not a caller input.
    /// `render_carry_positions` parses and skeleton-checks the plan
    /// with an opaque token at every carried `{{name}}` — the carried
    /// bytes never pass through the plan parser, then splices the
    /// verified material into the parsed ticket fields verbatim —
    /// while every other input still satisfies the one-line grammar.
    /// `carry` keys are declared inputs that must not collide with
    /// `inputs` and each must land in exactly one place.
    pub fn parse_carry(
        text: &str,
        inputs: &BTreeMap<String, String>,
        carry: &BTreeMap<String, String>,
    ) -> Result<Self> {
        let template = workflow::parse_template(text)?;
        let publication_slot = template.publication_slot;
        let capability_slots = template.capability_slots;
        let required_asset_slot = template.required_asset_slot;
        let execution = template.execution.as_str().to_string();
        let carries = template.carries;
        let (parsed, metadata) = workflow::render_carry_positions(text, inputs, carry)?;
        if parsed.tickets.len() > 16 {
            return Err(Error::rejected("local runs support at most 16 steps"));
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
                    if !matches!(key.trim(), "action" | "uses" | "tries" | "reviewer") {
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
            steps.push(LocalStep {
                id: format!("s{}", n + 1),
                kind: kind.into(),
                assignee,
                dependencies,
                instruction,
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
            source_digest: artifact_digest(text.as_bytes()),
            title: parsed.title,
            steps,
            publication_slot,
            capability_slots,
            required_asset_slot,
            execution,
            carries,
        })
    }
}

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS app_install_capabilities(
 install_id TEXT PRIMARY KEY, epoch INTEGER NOT NULL CHECK(epoch>0),
 digest TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('approved','revoked')),
 created REAL NOT NULL);
CREATE TABLE IF NOT EXISTS app_runs(
 id TEXT PRIMARY KEY, install_id TEXT NOT NULL, epoch INTEGER NOT NULL,
 bundle_digest TEXT NOT NULL, snapshot TEXT NOT NULL, snapshot_digest TEXT NOT NULL,
 project_link TEXT, owner_pm TEXT NOT NULL, request_id TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('awaiting_approval','approved','running','succeeded','failed','cancelled')),
 approved_digest TEXT, created REAL NOT NULL, updated REAL NOT NULL,
 UNIQUE(install_id,request_id));
CREATE TABLE IF NOT EXISTS app_run_failures(
 run_id TEXT PRIMARY KEY REFERENCES app_runs(id), step_id TEXT NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('refused','uncertain')), reason TEXT NOT NULL,
 created REAL NOT NULL);
CREATE TABLE IF NOT EXISTS app_run_steps(
 run_id TEXT NOT NULL REFERENCES app_runs(id), step_id TEXT NOT NULL,
 task_id TEXT NOT NULL UNIQUE REFERENCES tasks(id), spec TEXT NOT NULL,
 identity_digest TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('pending','dispatched','succeeded','failed')),
 message_id TEXT UNIQUE, result_digest TEXT,
 PRIMARY KEY(run_id,step_id));
CREATE TABLE IF NOT EXISTS app_run_artifacts(
 id TEXT PRIMARY KEY, run_id TEXT NOT NULL, step_id TEXT NOT NULL,
 message_id TEXT NOT NULL UNIQUE, turn_id TEXT NOT NULL, producer TEXT NOT NULL,
 digest TEXT NOT NULL, media_type TEXT NOT NULL, content BLOB NOT NULL,
 created REAL NOT NULL, UNIQUE(run_id,step_id),
 FOREIGN KEY(run_id,step_id) REFERENCES app_run_steps(run_id,step_id));
CREATE TABLE IF NOT EXISTS app_run_reviews(
 run_id TEXT NOT NULL, step_id TEXT NOT NULL, artifact_id TEXT NOT NULL REFERENCES app_run_artifacts(id),
 artifact_digest TEXT NOT NULL, reviewer TEXT NOT NULL, message_id TEXT UNIQUE NOT NULL,
 decision TEXT NOT NULL CHECK(decision IN ('approve','revise')), rationale TEXT NOT NULL,
 PRIMARY KEY(run_id,step_id), FOREIGN KEY(run_id,step_id) REFERENCES app_run_steps(run_id,step_id));
";

pub struct LocalRunProvenance<'a> {
    pub selected_source: Option<(&'a str, &'a str)>,
    pub input_origins: &'a BTreeMap<String, String>,
    /// CAD-1143 Redo carry request: the source run and the retained half.
    /// The frame names only these two; the store derives everything else
    /// from durable history inside the creation transaction.
    pub carry: Option<CarryRequest<'a>>,
}

/// CAD-1143 Redo carry: `retain_image` names the untouched half — a
/// retained image for Redo text, a retained caption for Redo image.
/// `carry_inputs` are the run-owned, digest-verified material values the
/// daemon resolved from durable history (`carry_caption` for a text
/// retain, `carry_asset_receipt_id` for an image retain). They are not
/// caller inputs: the plan is parsed with an opaque token at their
/// positions and the creation transaction asserts each byte against the
/// derived material before it is frozen into `snapshot.inputs`.
pub struct CarryRequest<'a> {
    pub from_run_id: &'a str,
    pub retain_image: bool,
    pub carry_inputs: &'a BTreeMap<String, String>,
}

impl Store {
    pub(super) fn refuse_app_task(conn: &impl super::StoreConn, task_id: &str) -> Result<()> {
        if conn
            .query_row(
                "SELECT 1 FROM app_run_steps WHERE task_id=?",
                [task_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Err(Error::rejected(
                "app-owned tasks use the app run lifecycle; generic task mutation is refused",
            ));
        }
        Ok(())
    }
    pub(super) fn refuse_app_job(conn: &impl super::StoreConn, job_id: &str) -> Result<()> {
        if conn.query_row("SELECT 1 FROM app_run_steps s JOIN tasks t ON t.id=s.task_id WHERE t.job_id=? LIMIT 1",[job_id], |_| Ok(())).optional()?.is_some() {
            return Err(Error::rejected("app-owned jobs use the app run lifecycle"));
        }
        Ok(())
    }
    pub fn app_capability_decide(&self, id: &str, digest: &str, approve: bool) -> Result<Value> {
        self.app_capability_decide_audited(id, digest, approve, json!({}))
    }
    /// CAD-1119: installing or updating an app is the operator's consent.
    /// The operator install/upgrade path records the approval of exactly
    /// that installed digest, auditing who installed it, how, and the
    /// capabilities it declares. An approval already in force for this
    /// digest is left untouched: re-deciding the same digest would
    /// invalidate effects staged under it.
    pub fn app_install_consent(
        &self,
        id: &str,
        digest: &str,
        via: &str,
        capabilities: &Value,
    ) -> Result<Option<Value>> {
        if self.app_capability_status(id, digest)?["state"] == "approved" {
            return Ok(None);
        }
        self.app_capability_decide_audited(
            id,
            digest,
            true,
            json!({"via": via, "capabilities": capabilities}),
        )
        .map(Some)
    }
    /// CAD-1129 H5: revoke this install's consent at its current
    /// digest — the soft-remove path. `app_capability_decide_audited`
    /// already writes the `revoked` epoch; this is the operator-facing
    /// wrapper that never leaves an `approved` slot behind.
    pub fn app_install_revoke(&self, id: &str, digest: &str, via: &str) -> Result<Value> {
        self.app_capability_decide_audited(id, digest, false, json!({"via": via}))
    }
    fn app_capability_decide_audited(
        &self,
        id: &str,
        digest: &str,
        approve: bool,
        audit: Value,
    ) -> Result<Value> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
        let previous: Option<(String, String)> = tx
            .query_opt(
                "SELECT digest,state FROM app_install_capabilities WHERE install_id=?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
        tx.execute("INSERT INTO app_install_capabilities VALUES(?,1,?,?,?) ON CONFLICT(install_id) DO UPDATE SET epoch=epoch+1,digest=excluded.digest,state=excluded.state,created=excluded.created",params![id,digest,if approve{"approved"}else{"revoked"},now()])?;
        let epoch: i64 = tx.query_row(
            "SELECT epoch FROM app_install_capabilities WHERE install_id=?",
            [id],
            |r| r.get(0),
        )?;
        if approve {
            // Reapproving the same bundle supersedes its older epochs. A new
            // bundle leaves completed old-version work authorized by its
            // exact historical epoch and retained bundle bytes.
            tx.execute(
                "UPDATE app_capability_epochs SET state='revoked' WHERE install_id=? AND digest=?",
                params![id, digest],
            )?;
            if previous.as_ref().is_some_and(|(old, _)| old == digest) {
                Self::app_effect_invalidate_in(&tx, id, None, None, Some(digest))?;
            }
        } else {
            // An explicit installation revoke removes authority from every
            // version, including completed historical work.
            tx.execute(
                "UPDATE app_capability_epochs SET state='revoked' WHERE install_id=?",
                [id],
            )?;
            Self::app_effect_invalidate_in(&tx, id, None, None, None)?;
        }
        tx.execute("INSERT INTO app_capability_epochs(install_id,epoch,digest,state,created) VALUES(?,?,?,?,?)",
            params![id,epoch,digest,if approve{"approved"}else{"revoked"},now()])?;
        Self::event(
            &tx,
            Self::DAEMON_STREAM,
            if approve {
                "app_install_capability_approved"
            } else {
                "app_install_capability_revoked"
            },
            {
                let mut event =
                    json!({"install_id":id,"digest":digest,"epoch":epoch,"actor":"operator"});
                if let (Some(event), Some(audit)) = (event.as_object_mut(), audit.as_object()) {
                    for (key, value) in audit {
                        event.entry(key.clone()).or_insert_with(|| value.clone());
                    }
                }
                event
            },
        )?;
        Ok(
            json!({"install_id":id,"epoch":epoch,"digest":digest,"approved":approve,"capabilities":["local.text.produce","local.text.review"],"outward_release":false}),
        )
        })
    }
}

impl Store {
    pub fn app_run_create(&self, request: LocalRunRequest<'_>) -> Result<Value> {
        self.app_run_create_with_context(request, None)
    }
    pub fn app_run_create_with_context(
        &self,
        request: LocalRunRequest<'_>,
        context: Option<&super::app_contexts::ContextProof>,
    ) -> Result<Value> {
        self.app_run_create_with_publication(request, context, None)
    }
    pub fn app_run_create_with_publication(
        &self,
        request: LocalRunRequest<'_>,
        context: Option<&super::app_contexts::ContextProof>,
        binding: Option<&super::app_bindings::BindingProof>,
    ) -> Result<Value> {
        self.app_run_create_with_capabilities(
            request,
            context,
            binding,
            &BTreeMap::new(),
            &BTreeMap::new(),
            LocalRunProvenance {
                selected_source: None,
                input_origins: &BTreeMap::new(),
                carry: None,
            },
        )
    }
    pub fn app_run_create_with_capabilities(
        &self,
        request: LocalRunRequest<'_>,
        context: Option<&super::app_contexts::ContextProof>,
        binding: Option<&super::app_bindings::BindingProof>,
        capabilities: &BTreeMap<String, super::app_bindings::BindingProof>,
        quotes: &BTreeMap<String, crate::platform::AppCapabilityQuote>,
        provenance: LocalRunProvenance<'_>,
    ) -> Result<Value> {
        let LocalRunProvenance {
            selected_source,
            input_origins,
            carry,
        } = provenance;
        let LocalRunRequest {
            install_id,
            bundle_digest,
            workflow,
            inputs,
            request_id,
            owner_pm,
            project_link,
        } = request;
        crate::proto::identifier(request_id, "app run request ID")?;
        let verified_source = selected_source
            .map(|(receipt, _)| self.app_capability_result(receipt))
            .transpose()?;
        self.write_tx(|conn| {

                    let tx = &mut *conn;
                    if let Some(proof) = context {
                        Self::app_context_proof_current_in(&tx, install_id, proof)?;
                    }
                    if let Some(proof) = binding {
                        let slot = workflow
                            .publication_slot
                            .as_deref()
                            .ok_or_else(|| Error::rejected("binding requires a selected publication slot"))?;
                        if proof.config["bundle_digest"] != bundle_digest
                            || !super::app_bindings::binding_current_in(
                                &tx,
                                install_id,
                                context.map(|c| c.id.as_str()),
                                slot,
                                proof,
                            )?
                        {
                            return Err(Error::rejected(
                                "publication binding is stale or belongs to a different scope",
                            ));
                        }
                    }
                    if capabilities.len() != workflow.capability_slots.len()
                        || quotes.len() != workflow.capability_slots.len()
                    {
                        return Err(Error::rejected(
                            "every declared run capability needs an exact binding",
                        ));
                    }
                    for slot in &workflow.capability_slots {
                        if !quotes
                            .get(slot)
                            .is_some_and(crate::platform::AppCapabilityQuote::valid)
                        {
                            return Err(Error::rejected(
                                "run capability needs a valid frozen price quote",
                            ));
                        }
                        let proof = capabilities
                            .get(slot)
                            .ok_or_else(|| Error::rejected("run capability binding is absent"))?;
                        if proof.config["bundle_digest"] != bundle_digest
                            || !super::app_bindings::binding_current_in(
                                &tx,
                                install_id,
                                context.map(|c| c.id.as_str()),
                                slot,
                                proof,
                            )?
                            || !matches!(
                                proof.config["mapping"]["effect"].as_str(),
                                Some("read" | "draft")
                            )
                        {
                            return Err(Error::rejected(
                                "run capability binding is stale or belongs to a different scope",
                            ));
                        }
                    }
                    let source = if let Some((receipt_id, post_id)) = selected_source {
                        let row = tx.query_opt(
                            "SELECT r.run_id,r.slot,r.binding_digest,r.result,r.result_digest,a.install_id,a.context_id,a.state
                             FROM app_capability_results r JOIN app_runs a ON a.id=r.run_id WHERE r.id=?",
                            [receipt_id], |r| Ok((
                                r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,
                                r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,
                                r.get::<_,Option<String>>(6)?,r.get::<_,String>(7)?
                            )),
                        )?.ok_or_else(||Error::rejected("source receipt is unavailable"))?;
                        if row.5 != install_id
                            || row.6.as_deref() != context.map(|c| c.id.as_str())
                            || !matches!(row.7.as_str(), "succeeded" | "failed")
                        {
                            return Err(Error::rejected(
                                "source receipt is outside this installation/context or incomplete",
                            ));
                        }
                        let source_run = Self::app_run_show_in(&tx, &row.0)?;
                        Self::app_completed_current_in(&tx, &source_run)?;
                        super::app_capabilities::source_receipt_recoverable_in(
                            &tx,
                            &source_run,
                            verified_source
                                .as_ref()
                                .ok_or_else(|| Error::rejected("source receipt is unavailable"))?,
                        )?;
                        let source_binding = &source_run["snapshot"]["capabilities"][&row.1];
                        if source_binding["digest"] != row.2
                            || source_binding["config"]["mapping"]["effect"] != "read"
                        {
                            return Err(Error::rejected(
                                "source receipt lacks a current read binding",
                            ));
                        }
                        let result: Value = serde_json::from_str(&row.3)?;
                        if verified_source.as_ref().is_none_or(|receipt| {
                            receipt["digest"] != row.4
                                || receipt["result"] != result
                                || receipt["run_id"] != row.0
                                || receipt["slot"] != row.1
                                || receipt["binding_digest"] != row.2
                        }) {
                            return Err(Error::rejected("selected source receipt has changed"));
                        }
                        let posts = result["posts"]
                            .as_array()
                            .ok_or_else(|| Error::rejected("source receipt has no normalized posts"))?;
                        let mut matches = posts
                            .iter()
                            .filter(|post| post["id"].as_str() == Some(post_id));
                        let selected = matches
                            .next()
                            .ok_or_else(|| Error::rejected("selected post is absent from source receipt"))?;
                        if matches.next().is_some() {
                            return Err(Error::rejected(
                                "selected post id is ambiguous in source receipt",
                            ));
                        }
                        let caption = selected["caption"]
                            .as_str()
                            .ok_or_else(|| Error::rejected("selected post has no normalized caption"))?;
                        let display = workflow::source_input_line(caption)?;
                        if inputs.get("source").map(String::as_str) != Some(display.as_str()) {
                            return Err(Error::rejected(
                                "run source input differs from the selected provider post",
                            ));
                        }
                        Some(json!({"receipt_id":receipt_id,"receipt_digest":row.4,
                            "source_run_id":row.0,"binding_digest":row.2,
                            "post_id":post_id,"post":selected,
                            "post_digest":material_digest(selected)}))
                    } else {
                        None
                    };
                    // CAD-1143 Redo carry: derive the retained half from
                    // durable history inside this transaction. Render-time
                    // seeding (daemon, pre-freeze) must equal the derived
                    // material — asserted here like the selected-post display
                    // check; subject is overwritten authoritatively when
                    // frozen. Refuses BEFORE anything is frozen. The record
                    // is digest-bound once the snapshot below freezes.
                    let mut seeded: Option<BTreeMap<String, String>> = None;
                    let mut source_copy: Option<Value> = None;
                    let carry_record = if let Some(request) = carry {
                        if selected_source.is_some() {
                            return Err(Error::rejected(
                                "carry and source selection conflict; provenance is one or the other",
                            ));
                        }
                        let retain = if request.retain_image { "image" } else { "text" };
                        if !workflow.carries.iter().any(|half| half == retain) {
                            return Err(Error::rejected(
                                "workflow does not carry the requested half",
                            ));
                        }
                        let material = Self::app_carry_material_in(
                            &tx,
                            install_id,
                            context.map(|c| c.id.as_str()),
                            request.from_run_id,
                            request.retain_image,
                        )?;
                        let mut effective = inputs.clone();
                        // The render-time seeding above must equal this
                        // transaction's derived material (mirrors the
                        // selected-source display check); subject is
                        // overwritten authoritatively when frozen.
                        if effective.get("source").map(String::as_str)
                            != material["source_facts"].as_str()
                        {
                            return Err(Error::rejected(
                                "carry source input differs from frozen source facts",
                            ));
                        }
                        // The retained half travels in `carry_inputs` —
                        // run-owned material resolved daemon-side, never a
                        // caller input. Assert each carried byte against the
                        // transaction's own derived material, then seed it
                        // into the frozen inputs so the render reflects the
                        // exact reviewed bytes. A text retain carries the
                        // caption verbatim; an image retain carries the
                        // pinned receipt id. Nothing else may be carried.
                        let expected_carry: BTreeMap<String, String> = if !request.retain_image {
                            let caption = material["artifact"]["text"]
                                .as_str()
                                .ok_or_else(|| {
                                    Error::rejected("carry source caption is unavailable")
                                })?;
                            BTreeMap::from([("carry_caption".to_string(), caption.to_string())])
                        } else {
                            let receipt = material["asset"]["receipt_id"]
                                .as_str()
                                .ok_or_else(|| {
                                    Error::rejected("carry source review pins no image")
                                })?;
                            BTreeMap::from([(
                                "carry_asset_receipt_id".to_string(),
                                receipt.to_string(),
                            )])
                        };
                        if request.carry_inputs != &expected_carry {
                            return Err(Error::rejected(
                                "carried material differs from frozen carry bytes",
                            ));
                        }
                        for (key, value) in request.carry_inputs {
                            effective.insert(key.clone(), value.clone());
                        }
                        if let Some(subject) = material["source_subject"].as_str() {
                            effective.insert("subject".into(), subject.to_owned());
                        }
                        if serde_json::to_vec(&effective)
                            .map_err(|e| Error::internal(e.to_string()))?
                            .len()
                            > 32 * 1024
                        {
                            return Err(Error::rejected(
                                "effective inputs exceed encoded byte limit",
                            ));
                        }
                        seeded = Some(effective);
                        // Redo-image generation needs the broker's
                        // selected-source provenance: carry the source
                        // snapshot's frozen record forward, explicitly
                        // marked (never a fresh selection). Manual-fact
                        // sources have none; the broker's facts path covers
                        // them. No provider read is implied or charged.
                        if !request.retain_image {
                            if let Some(origin) = material
                                .get("source_origin")
                                .filter(|provenance| provenance.is_object())
                            {
                                let mut carried = origin.clone();
                                if let Some(object) = carried.as_object_mut() {
                                    object.insert(
                                        "carried_from_run_id".into(),
                                        json!(request.from_run_id),
                                    );
                                }
                                source_copy = Some(carried);
                            }
                        }
                        Some(json!({
                            "from_run_id": request.from_run_id,
                            "retain": retain,
                            "source_bundle_digest": material["source_bundle_digest"],
                            "source_snapshot_digest": material["source_snapshot_digest"],
                            "artifact_id": material["artifact"]["id"],
                            "artifact_digest": material["artifact"]["digest"],
                            "asset_receipt_id": material["asset"]["receipt_id"],
                            "asset_digest": material["asset"]["digest"],
                            "asset_producer_step": material["asset"]["producer_step"],
                            "asset_slot": material["asset"]["slot"],
                            "asset_binding_digest": material["asset"]["binding_digest"],
                        }))
                    } else {
                        None
                    };
                    let epoch:i64=tx.query_opt("SELECT epoch FROM app_install_capabilities WHERE install_id=? AND digest=? AND state='approved'",params![install_id,bundle_digest],|r|r.get(0))?.ok_or_else(||Error::rejected("installation capability approval is absent or stale"))?;
                    let mut assignments = BTreeMap::new();
                    if workflow.execution == "host" {
                        // CAD-1171: the operator's own click runs the
                        // capability; there is no owner PM and no worker.
                        if owner_pm.is_some() {
                            return Err(Error::rejected(
                                "a host-execution run has no owner PM",
                            ));
                        }
                    } else {
                    let owner_pm = owner_pm.ok_or_else(|| {
                        Error::rejected("a run needs its owner PM")
                    })?;
                    let owner = self.agent_in(&tx, owner_pm)?;
                    if owner.role != "pm" {
                        return Err(Error::rejected("run owner must be an existing PM"));
                    }
                    for step in &workflow.steps {
                        let agent = self.agent_in(&tx, &step.assignee)?;
                        if !(agent.enabled || Self::agent_auto_parked_in(&tx, &agent)?)
                            || agent.role != "worker"
                            || !matches!(
                                (agent.provider.as_str(), agent.endpoint_kind.as_str()),
                                ("codex", "managed" | "managed-ws")
                                    | ("claude", "managed")
                                    | ("pi", "managed")
                                    | ("fake", "fake")
                            )
                            || !crate::adapter::registry::spec_opt(&agent.provider, &agent.endpoint_kind)
                                .is_some_and(|s| s.has_actor)
                            || agent.session_id.is_none()
                        {
                            return Err(Error::rejected(
                                "local team needs an enabled registered managed local worker; PTY and remote endpoints are unsupported",
                            ));
                        }
                        let generation = material_digest(&Self::app_binding_identity(&agent));
                        let group = agent
                            .params
                            .as_ref()
                            .and_then(|p| p.get("upstream"))
                            .and_then(Value::as_str);
                        if agent.alias == owner_pm || group != Some(owner_pm) {
                            return Err(Error::rejected(
                                "local worker must belong to the run owner group",
                            ));
                        }
                        assignments.insert(step.id.clone(),json!({"alias":agent.alias,"identity_digest":generation,"identity":Self::app_binding_identity(&agent),"role":agent.role,"provider":agent.provider,"endpoint_kind":agent.endpoint_kind}));
                    }
                    }
                    let mut snapshot = json!({"schema":1,"install_id":install_id,"bundle_digest":bundle_digest,"epoch":epoch,"workflow":workflow,"inputs":seeded.as_ref().unwrap_or(inputs),"assignments":assignments,"owner_pm":owner_pm,"project_link":project_link,"artifact_policy":{"types":["text/plain","text/markdown"],"max_bytes":ARTIFACT_BYTES,"aggregate_bytes":RUN_ARTIFACT_BYTES}});
                    if !input_origins.is_empty() {
                        snapshot["input_origins"] = json!(input_origins);
                    }
                    if let Some(proof) = context {
                        snapshot["schema"] = json!(2);
                        snapshot["context"] =
                            json!({"id":proof.id,"revision":proof.revision,"digest":proof.digest});
                    }
                    if let Some(slot) = &workflow.publication_slot {
                        snapshot["schema"] = json!(3);
                        if context.is_none() {
                            snapshot["context"] = Value::Null;
                        }
                        snapshot["publication"] = json!({"slot":slot,"binding":binding});
                    }
                    if !workflow.capability_slots.is_empty() {
                        snapshot["schema"] = json!(4);
                        if context.is_none() {
                            snapshot["context"] = Value::Null;
                        }
                        snapshot["capabilities"] = json!(capabilities);
                        snapshot["quotes"] = json!(quotes);
                    }
                    if let Some(source) = source {
                        snapshot["schema"] = json!(4);
                        if context.is_none() {
                            snapshot["context"] = Value::Null;
                        }
                        snapshot["capabilities"] = json!(capabilities);
                        snapshot["quotes"] = json!(quotes);
                        snapshot["workflow"]["capability_slots"] = json!(workflow.capability_slots);
                        snapshot["source"] = source;
                    }
                    // Schema stays 4: capability turns and the publication
                    // path keep working unchanged; the `carry` record itself
                    // marks the redo and is digest-bound by the freeze below.
                    if let Some(record) = carry_record {
                        snapshot["carry"] = record;
                    }
                    // Redo-image generation needs the broker's selected-source
                    // provenance: carry the source snapshot's frozen record
                    // forward, explicitly marked (never a fresh selection).
                    // Manual-fact sources have none — the broker's facts path
                    // covers them. No provider read is implied or charged.
                    if let Some(origin) = source_copy {
                        snapshot["source"] = origin;
                    }
                    let digest = material_digest(&snapshot);
                    if let Some((id, existing)) = tx
                        .query_opt(
                            "SELECT id,snapshot_digest FROM app_runs WHERE install_id=? AND request_id=?",
                            params![install_id, request_id],
                            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                        )?
                    {
                        if existing != digest {
                            return Err(Error::rejected(
                                "request ID already has a different immutable snapshot",
                            ));
                        }
                        return Self::app_run_show_in(&tx, &id);
                    }
                    let id = format!("run-{}", uuid::Uuid::new_v4().simple());
                    // CAD-1171: a host run stores an empty owner (its
                    // snapshot carries `owner_pm: null`).
                    let owner_row = owner_pm.unwrap_or_default();
                    tx.execute("INSERT INTO jobs(id,title,spec_path,spec_sha256,pm_alias,state,max_revisions,created,updated) VALUES(?,?,?,?,?,'open',1,?,?)",params![id,"App run","app-run",digest,owner_row,now(),now()])?;
                    tx.execute(
                        "INSERT INTO app_runs(id,install_id,epoch,bundle_digest,snapshot,snapshot_digest,project_link,owner_pm,request_id,state,approved_digest,created,updated,context_id) VALUES(?,?,?,?,?,?,?,?,?,'awaiting_approval',NULL,?,?,?)",
                        params![
                            id,
                            install_id,
                            epoch,
                            bundle_digest,
                            snapshot.to_string(),
                            digest,
                            project_link,
                            owner_row,
                            request_id,
                            now(),
                            now(),
                            context.map(|proof|proof.id.as_str())
                        ],
                    )?;
                    for step in &workflow.steps {
                        let task = format!("{id}-{}", step.id);
                        // CAD-1171: a host capability step has no assignment.
                        let generation = assignments
                            .get(&step.id)
                            .and_then(|row| row["identity_digest"].as_str())
                            .unwrap_or_default()
                            .to_string();
                        tx.execute("INSERT INTO tasks(id,job_id,role,assignee,state,created,updated) VALUES(?,?,?,?,'draft',?,?)",params![task,id,step.kind,step.assignee,now(),now()])?;
                        tx.execute(
                            "INSERT INTO app_run_steps VALUES(?,?,?,?,?,'pending',NULL,NULL)",
                            params![
                                id,
                                step.id,
                                task,
                                serde_json::to_string(step).map_err(|e| Error::internal(e.to_string()))?,
                                generation
                            ],
                        )?;
                    }
                    Self::event(
                        &tx,
                        Self::DAEMON_STREAM,
                        "app_run_created",
                        json!({"run_id":id,"install_id":install_id,"snapshot_digest":digest,"actor":"operator"}),
                    )?;
                    Self::app_run_show_in(&*tx, &id)
        })
    }
    pub fn app_run_show(&self, id: &str) -> Result<Value> {
        self.read_tx(|conn| Self::app_run_show_in(&conn, id))
    }
    pub(super) fn app_run_show_in(conn: &impl super::StoreConn, id: &str) -> Result<Value> {
        let mut value=conn.query_row("SELECT install_id,epoch,snapshot,snapshot_digest,project_link,state,approved_digest FROM app_runs WHERE id=?",[id],|r|Ok(json!({"id":id,"install_id":r.get::<_,String>(0)?,"epoch":r.get::<_,i64>(1)?,"snapshot":r.get::<_,String>(2)?,"snapshot_digest":r.get::<_,String>(3)?,"project_link":r.get::<_,Option<String>>(4)?,"state":r.get::<_,String>(5)?,"approved_digest":r.get::<_,Option<String>>(6)?}))).optional()?.ok_or_else(||Error::rejected("unknown app run"))?;
        value["snapshot"] = serde_json::from_str(value["snapshot"].as_str().unwrap())
            .map_err(|e| Error::internal(e.to_string()))?;
        value["context_id"] = json!(conn.query_row(
            "SELECT context_id FROM app_runs WHERE id=?",
            [id],
            |r| r.get::<_, Option<String>>(0)
        )?);
        let (created, updated): (f64, f64) = conn.query_row(
            "SELECT created,updated FROM app_runs WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        value["created"] = json!(created as i64);
        value["updated"] = json!(updated as i64);
        // CAD-1123 R4: the latest approval of this run, when recorded.
        if let Some((payload, at)) = conn
            .query_row(
                "SELECT payload,at FROM events WHERE job_id=?1 AND alias=?2 AND kind='app_run_approved' ORDER BY seq DESC LIMIT 1",
                params![id, APP_RUN_APPROVAL_STREAM],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)),
            )
            .optional()?
        {
            let by = serde_json::from_str::<Value>(&payload)
                .ok()
                .and_then(|v| v["by"].as_str().map(str::to_string))
                .unwrap_or_else(|| "operator".into());
            value["approval"] = json!({"by": by, "at": at as i64});
        }
        if let Some((kind, reason, step_id)) = conn
            .query_row(
                "SELECT kind,reason,step_id FROM app_run_failures WHERE run_id=?",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
        {
            value["failure"] = json!({"kind":kind,"reason":reason,"step_id":step_id});
        }
        value["steps"]=Value::Array(conn.query_vec("SELECT step_id,task_id,state,message_id FROM app_run_steps WHERE run_id=? ORDER BY step_id",[id],|r|Ok(json!({"step_id":r.get::<_,String>(0)?,"task_id":r.get::<_,String>(1)?,"state":r.get::<_,String>(2)?,"message_id":r.get::<_,Option<String>>(3)?})))?);
        value["artifacts"]=Value::Array(conn.query_vec("SELECT id,step_id,digest,media_type,length(content) FROM app_run_artifacts WHERE run_id=? ORDER BY step_id",[id],|r|Ok(json!({"id":r.get::<_,String>(0)?,"step_id":r.get::<_,String>(1)?,"digest":r.get::<_,String>(2)?,"media_type":r.get::<_,String>(3)?,"size":r.get::<_,i64>(4)?})))?);
        value["reviews"]=Value::Array(conn.query_vec("SELECT step_id,artifact_digest,reviewer,decision,rationale,asset_receipt_id,asset_digest FROM app_run_reviews WHERE run_id=?",[id],|r|{
            let mut review=json!({"step_id":r.get::<_,String>(0)?,"artifact_digest":r.get::<_,String>(1)?,"reviewer":r.get::<_,String>(2)?,"decision":r.get::<_,String>(3)?,"rationale":r.get::<_,String>(4)?});
            let asset:Option<String>=r.get(5)?;
            let digest:Option<String>=r.get(6)?;
            if let (Some(asset),Some(digest))=(asset,digest) {
                review["asset_receipt_id"]=json!(asset);
                review["asset_digest"]=json!(digest);
            }
            Ok(review)
        })?);
        Ok(value)
    }
    pub fn app_run_list(&self, install_id: Option<&str>) -> Result<Value> {
        self.app_run_list_filtered(install_id, None)
    }
    pub fn app_run_list_filtered(
        &self,
        install_id: Option<&str>,
        context_id: Option<&str>,
    ) -> Result<Value> {
        if let Some(context) = context_id {
            self.app_context_show(
                install_id
                    .ok_or_else(|| Error::rejected("context run filter requires installation"))?,
                context,
            )?;
        }
        let ids = self
            .conn()
            .prepare(
                "SELECT id FROM app_runs WHERE (?1 IS NULL OR install_id=?1) AND (?2 IS NULL OR context_id=?2) ORDER BY created",
            )?
            .query_map(params![install_id,context_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!({"runs":ids.iter().map(|id|self.app_run_show(id)).collect::<Result<Vec<_>>>()?}))
    }
    pub fn app_run_decide(
        &self,
        id: &str,
        digest: Option<&str>,
        cancel: bool,
        current_bundle: Option<&str>,
    ) -> Result<Value> {
        self.write_tx(|conn| {

                    let tx = &mut *conn;
                    let run = Self::app_run_show_in(&tx, id)?;
                    let state = run["state"].as_str().unwrap();
                    if cancel {
                        if matches!(state, "succeeded" | "failed" | "cancelled") {
                            return Err(Error::rejected("run is already terminal"));
                        }
                        tx.execute(
                            "UPDATE app_runs SET state='cancelled',approved_digest=NULL,updated=? WHERE id=?",
                            params![now(), id],
                        )?;
                        tx.execute(
                            "UPDATE jobs SET state='cancelled',updated=? WHERE id=?",
                            params![now(), id],
                        )?;
                    } else {
                        if state != "awaiting_approval" || digest != run["snapshot_digest"].as_str() {
                            return Err(Error::rejected(
                                "execution decision needs the pending immutable snapshot digest",
                            ));
                        }
                        Self::app_current_in(
                            &tx,
                            &run,
                            current_bundle.ok_or_else(|| {
                                Error::rejected("execution approval requires current installation proof")
                            })?,
                        )?;
                        tx.execute("UPDATE app_runs SET state='approved',approved_digest=snapshot_digest,updated=? WHERE id=?",params![now(),id])?;
                        // CAD-1123 R4: a durable, run-indexed approval record on its
                        // own stream (the daemon stream is pruned). The actor is the
                        // daemon-derived class, never a caller-supplied name.
                        Self::event_scoped(
                            &tx,
                            APP_RUN_APPROVAL_STREAM,
                            "app_run_approved",
                            json!({"run_id":id,"digest":run["snapshot_digest"],"by":"operator"}),
                            Some(id),
                            None,
                        )?;
                    }
                    Self::event(
                        &tx,
                        Self::DAEMON_STREAM,
                        if cancel {
                            "app_run_cancelled"
                        } else {
                            "app_run_execution_approved"
                        },
                        json!({"run_id":id,"digest":run["snapshot_digest"],"actor":"operator"}),
                    )?;
                    Self::app_run_show_in(&tx, id)
        })
    }
    pub(super) fn app_current_in(
        conn: &impl super::StoreConn,
        run: &Value,
        bundle: &str,
    ) -> Result<()> {
        Self::app_authority_in(conn, run, bundle, false)
    }
    /// Completed material retains its original approval epoch across package
    /// upgrades. It is never used for a new/active run or worker dispatch.
    pub(super) fn app_completed_current_in(
        conn: &impl super::StoreConn,
        run: &Value,
    ) -> Result<()> {
        if !matches!(run["state"].as_str(), Some("succeeded" | "failed"))
            || run["approved_digest"] != run["snapshot_digest"]
        {
            return Err(Error::rejected(
                "historical run is not completed and approved",
            ));
        }
        let bundle = run["snapshot"]["bundle_digest"]
            .as_str()
            .ok_or_else(|| Error::rejected("historical bundle digest is missing"))?;
        Self::app_authority_in(conn, run, bundle, true)
    }
    fn app_authority_in(
        conn: &impl super::StoreConn,
        run: &Value,
        bundle: &str,
        historical: bool,
    ) -> Result<()> {
        Self::app_context_current_in(conn, run)?;
        if material_digest(&run["snapshot"]) != run["snapshot_digest"] {
            return Err(Error::rejected(
                "immutable app run snapshot receipt is corrupt",
            ));
        }
        // CAD-1143 Redo carry record: digest-bound by the freeze checked
        // above; shape-checked here so every currentness gate (approval,
        // dispatch, fetch, results) sees it. Ordinary snapshots skip this.
        if run["snapshot"].get("carry").is_some() {
            let carry = &run["snapshot"]["carry"];
            let bound = carry
                .get("from_run_id")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.is_empty())
                && matches!(
                    carry.get("retain").and_then(Value::as_str),
                    Some("image" | "text")
                )
                && carry
                    .get("artifact_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| !id.is_empty())
                && carry
                    .get("artifact_digest")
                    .and_then(Value::as_str)
                    .is_some_and(|digest| !digest.is_empty());
            if !bound {
                return Err(Error::rejected(
                    "carry record is missing its digest-bound provenance",
                ));
            }
        }
        if run["snapshot"]["schema"] == 3
            || (run["snapshot"]["schema"] == 4 && run["snapshot"]["publication"].is_object())
        {
            let slot = run["snapshot"]["publication"]["slot"]
                .as_str()
                .ok_or_else(|| Error::rejected("publication snapshot slot is missing"))?;
            if run["snapshot"]["workflow"]["publication_slot"].as_str() != Some(slot) {
                return Err(Error::rejected(
                    "publication slot differs from its frozen workflow",
                ));
            }
            match run["snapshot"]["publication"].get("binding") {
                Some(Value::Null) => {}
                Some(value) => {
                    let proof: super::app_bindings::BindingProof =
                        serde_json::from_value(value.clone()).map_err(|_| {
                            Error::rejected("publication binding snapshot is invalid")
                        })?;
                    if proof.config["bundle_digest"] != bundle
                        || !super::app_bindings::binding_current_in(
                            conn,
                            run["install_id"].as_str().unwrap(),
                            run["context_id"].as_str(),
                            slot,
                            &proof,
                        )?
                    {
                        return Err(Error::rejected("publication binding snapshot is stale"));
                    }
                }
                None => return Err(Error::rejected("publication binding snapshot is missing")),
            }
        } else if run["snapshot"].get("publication").is_some()
            || run["snapshot"]["workflow"]
                .get("publication_slot")
                .is_some()
        {
            return Err(Error::rejected(
                "legacy snapshot cannot carry publication authority",
            ));
        }
        if run["snapshot"]["schema"] != 4
            && run["snapshot"]["workflow"]
                .get("required_asset_slot")
                .is_some()
            && run["snapshot"]["carry"]["retain"].as_str() != Some("image")
        {
            return Err(Error::rejected(
                "required asset slot needs a capability snapshot",
            ));
        }
        if run["snapshot"]["schema"] == 4 {
            let declared = run["snapshot"]["workflow"]["capability_slots"]
                .as_array()
                .ok_or_else(|| {
                    Error::rejected("capability slots are missing from frozen workflow")
                })?;
            if let Some(required) = run["snapshot"]["workflow"].get("required_asset_slot") {
                // A carried image satisfies the required slot from proven
                // retained custody instead of generated capabilities; the
                // review intake still pins the frozen receipt (never fresh).
                let carried = required.as_str() == Some("image")
                    && run["snapshot"]["carry"]["retain"].as_str() == Some("image");
                if !carried {
                    required
                        .as_str()
                        .filter(|slot| declared.iter().any(|candidate| candidate == *slot))
                        .ok_or_else(|| {
                            Error::rejected("required asset slot differs from frozen capabilities")
                        })?;
                }
            }
            let frozen = run["snapshot"]["capabilities"]
                .as_object()
                .ok_or_else(|| Error::rejected("capability binding map is missing"))?;
            let quotes = run["snapshot"]["quotes"]
                .as_object()
                .ok_or_else(|| Error::rejected("capability price quote map is missing"))?;
            if (declared.is_empty() && run["snapshot"]["source"].is_null())
                || declared.len() != frozen.len()
                || declared.len() != quotes.len()
                || declared.len() > 8
            {
                return Err(Error::rejected(
                    "frozen capability slot set differs from workflow",
                ));
            }
            for name in declared {
                let slot = name
                    .as_str()
                    .ok_or_else(|| Error::rejected("frozen capability slot is invalid"))?;
                let quote: crate::platform::AppCapabilityQuote = serde_json::from_value(
                    quotes
                        .get(slot)
                        .ok_or_else(|| Error::rejected("frozen capability quote is missing"))?
                        .clone(),
                )
                .map_err(|_| Error::rejected("frozen capability quote is invalid"))?;
                if !quote.valid() {
                    return Err(Error::rejected("frozen capability quote is invalid"));
                }
                let proof: super::app_bindings::BindingProof = serde_json::from_value(
                    frozen
                        .get(slot)
                        .ok_or_else(|| Error::rejected("frozen capability binding is missing"))?
                        .clone(),
                )
                .map_err(|_| Error::rejected("frozen capability binding is invalid"))?;
                if proof.config["bundle_digest"] != bundle
                    || !matches!(
                        proof.config["mapping"]["effect"].as_str(),
                        Some("read" | "draft")
                    )
                    || !super::app_bindings::binding_current_in(
                        conn,
                        run["install_id"].as_str().unwrap(),
                        run["context_id"].as_str(),
                        slot,
                        &proof,
                    )?
                {
                    return Err(Error::rejected("frozen capability binding is stale"));
                }
            }
        } else if run["snapshot"].get("capabilities").is_some()
            || run["snapshot"]["workflow"]
                .get("capability_slots")
                .is_some_and(|v| !v.as_array().is_some_and(Vec::is_empty))
        {
            return Err(Error::rejected(
                "legacy snapshot cannot carry capability authority",
            ));
        }
        let table = if historical {
            "app_capability_epochs"
        } else {
            "app_install_capabilities"
        };
        let found=conn.query_row(&format!("SELECT 1 FROM {table} WHERE install_id=? AND epoch=? AND digest=? AND state='approved'"),params![run["install_id"].as_str(),run["epoch"].as_i64(),bundle], |_|Ok(())).optional()?;
        if found.is_none() || run["snapshot"]["bundle_digest"].as_str() != Some(bundle) {
            return Err(Error::rejected(
                "app capability epoch or bundle digest is stale",
            ));
        }
        Ok(())
    }
    fn app_context_current_in(conn: &impl super::StoreConn, run: &Value) -> Result<()> {
        let context = run["context_id"].as_str();
        if let Some(id) = context {
            let snapshot = &run["snapshot"];
            if !matches!(snapshot["schema"].as_u64(), Some(2..=4))
                || snapshot["context"]["id"].as_str() != Some(id)
            {
                return Err(Error::rejected("context snapshot association is invalid"));
            }
            let proof = super::app_contexts::ContextProof {
                id: id.to_string(),
                install_id: run["install_id"].as_str().unwrap().to_string(),
                revision: snapshot["context"]["revision"]
                    .as_i64()
                    .ok_or_else(|| Error::rejected("context snapshot revision is invalid"))?,
                digest: snapshot["context"]["digest"]
                    .as_str()
                    .ok_or_else(|| Error::rejected("context snapshot digest is invalid"))?
                    .to_string(),
            };
            Self::app_context_proof_current_in(conn, run["install_id"].as_str().unwrap(), &proof)?;
        } else if !((run["snapshot"]["schema"] == 1 && run["snapshot"].get("context").is_none())
            || (matches!(run["snapshot"]["schema"].as_u64(), Some(3 | 4))
                && run["snapshot"].get("context").is_some_and(Value::is_null)))
        {
            return Err(Error::rejected(
                "context-free snapshot association is invalid",
            ));
        }
        Ok(())
    }
}

impl Store {
    /// CAD-1120: the identity an app run binds a worker to — the
    /// registered agent and its native session, not one endpoint process.
    /// `generation` is left out: every open mints a new one, so binding
    /// it would refuse a worker that the idle timer stopped and dispatch
    /// woke. Each turn is still bound to the live generation by
    /// `local_token_current`.
    pub(super) fn app_binding_identity(agent: &Agent) -> Value {
        let mut identity = Self::agent_identity(agent);
        if let Some(fields) = identity.as_object_mut() {
            fields.remove("generation");
        }
        identity
    }

    /// CAD-1120: whether `agent` is parked by the idle timer rather than
    /// stopped by a person. It must be disabled and `stopped`, and its
    /// newest stop record must be the timer's own `agent_auto_stopped`.
    /// An operator or PM stop (`stop_requested`), an open (`ready`), or an
    /// auto-resume in flight or failed, written after it, makes this
    /// false. Only the daemon's idle timer writes that record, so no
    /// caller can mark a stopped worker as parked. A parked worker is
    /// woken by the CAD-413 auto-resume once its kickoff is queued.
    pub(super) fn agent_auto_parked_in(
        conn: &impl super::StoreConn,
        agent: &Agent,
    ) -> Result<bool> {
        if agent.enabled || agent.state != "stopped" {
            return Ok(false);
        }
        let placeholders = vec!["?"; AUTO_STOP_MARKER_KINDS.len()].join(",");
        let sql = format!(
            "SELECT kind FROM events WHERE alias=? AND kind IN ({placeholders})
             ORDER BY seq DESC LIMIT 1"
        );
        let mut args: Vec<&dyn rusqlite::ToSql> = vec![&agent.alias];
        args.extend(
            AUTO_STOP_MARKER_KINDS
                .iter()
                .map(|kind| kind as &dyn rusqlite::ToSql),
        );
        let newest: Option<String> = conn
            .query_row(&sql, args.as_slice(), |row| row.get(0))
            .optional()?;
        Ok(newest.as_deref() == Some(AUTO_STOP_EVENT))
    }
}

impl Store {
    /// Called only while the daemon holds the installation's PM lock. SQL
    /// rechecks the epoch and dependency state in the enqueue transaction.
    /// CAD-1171: complete the host step only after every declared capability
    /// slot has a retained receipt. The operator's click ran the provider;
    /// there is no worker, message or artifact.
    pub fn app_run_host_step_succeeded(&self, run_id: &str, step_id: &str) -> Result<Value> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let run = Self::app_run_show_in(tx, run_id)?;
            if run["state"] != "running"
                || run["approved_digest"] != run["snapshot_digest"]
                || run["snapshot"]["workflow"]["execution"] != "host"
            {
                return Err(Error::rejected("host run is not active and approved"));
            }
            let slots = run["snapshot"]["workflow"]["capability_slots"]
                .as_array()
                .ok_or_else(|| Error::rejected("host run has no required capability slots"))?;
            if slots.is_empty() {
                return Err(Error::rejected("host run has no required capability slots"));
            }
            let (task_id, step_state): (String, String) = tx
                .query_row(
                    "SELECT task_id,state FROM app_run_steps WHERE run_id=? AND step_id=?",
                    params![run_id, step_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(|_| Error::rejected("host step is absent"))?;
            if step_state != "dispatched" {
                return Err(Error::rejected("host step is not dispatched"));
            }
            let mut receipt_digests = Vec::with_capacity(slots.len());
            for slot in slots {
                let slot = slot
                    .as_str()
                    .ok_or_else(|| Error::rejected("host capability slot is invalid"))?;
                let digest: Option<String> = tx.query_row(
                    "SELECT result_digest FROM app_capability_results
                     WHERE run_id=? AND step_id=? AND slot=?",
                    params![run_id, step_id, slot],
                    |r| r.get(0),
                ).optional()?;
                let digest = digest.ok_or_else(|| {
                    Error::rejected("host step is missing a required capability receipt")
                })?;
                receipt_digests.push(json!({"slot":slot,"digest":digest}));
            }
            let result_digest = material_digest(&json!(receipt_digests));
            let now = now();
            tx.execute(
                "UPDATE app_run_steps SET state='succeeded',result_digest=? WHERE run_id=? AND step_id=? AND state='dispatched'",
                params![result_digest, run_id, step_id],
            )?;
            tx.execute(
                "UPDATE tasks SET state='done',error=NULL,updated=? WHERE id=?",
                params![now, task_id],
            )?;
            let pending: i64 = tx.query_row(
                "SELECT COUNT(*) FROM app_run_steps WHERE run_id=? AND state!='succeeded'",
                [run_id],
                |r| r.get(0),
            )?;
            if pending == 0 {
                tx.execute(
                    "UPDATE app_runs SET state='succeeded',updated=? WHERE id=? AND state='running'",
                    params![now, run_id],
                )?;
                tx.execute(
                    "UPDATE jobs SET state='done',updated=? WHERE id=?",
                    params![now, run_id],
                )?;
            }
            Self::event(
                tx,
                Self::DAEMON_STREAM,
                if pending == 0 {
                    "app_run_completed"
                } else {
                    "app_run_step_result_recorded"
                },
                json!({"run_id":run_id,"step_id":step_id,"host":true,"slots":slots.len()}),
            )?;
            Self::app_run_show_in(tx, run_id)
        })
    }

    /// A host capability refusal or uncertain result is terminal. Retain the
    /// claim and any earlier slot receipts, and never make it retryable via the
    /// background advance tick.
    pub fn app_run_host_step_failed(
        &self,
        run_id: &str,
        step_id: &str,
        kind: &str,
        reason: &str,
    ) -> Result<Value> {
        if !matches!(kind, "refused" | "uncertain") || reason.is_empty() || reason.len() > 1024 {
            return Err(Error::rejected("host failure record is invalid"));
        }
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let run = Self::app_run_show_in(tx, run_id)?;
            if run["state"] != "running"
                || run["approved_digest"] != run["snapshot_digest"]
                || run["snapshot"]["workflow"]["execution"] != "host"
            {
                return Err(Error::rejected("host run is not active and approved"));
            }
            let task_id: String = tx
                .query_row(
                    "SELECT task_id FROM app_run_steps WHERE run_id=? AND step_id=? AND state='dispatched'",
                    params![run_id, step_id],
                    |r| r.get(0),
                )
                .map_err(|_| Error::rejected("host step is not dispatched"))?;
            let now = now();
            tx.execute(
                "INSERT INTO app_run_failures(run_id,step_id,kind,reason,created) VALUES(?,?,?,?,?)",
                params![run_id, step_id, kind, reason, now],
            )?;
            tx.execute(
                "UPDATE app_run_steps SET state='failed' WHERE run_id=? AND step_id=? AND state='dispatched'",
                params![run_id, step_id],
            )?;
            tx.execute(
                "UPDATE tasks SET state='failed',error=?,updated=? WHERE id=?",
                params![reason, now, task_id],
            )?;
            tx.execute(
                "UPDATE app_runs SET state='failed',approved_digest=NULL,updated=? WHERE id=? AND state='running'",
                params![now, run_id],
            )?;
            tx.execute(
                "UPDATE jobs SET state='failed',updated=? WHERE id=?",
                params![now, run_id],
            )?;
            Self::event(
                tx,
                Self::DAEMON_STREAM,
                "app_run_host_step_failed",
                json!({"run_id":run_id,"step_id":step_id,"kind":kind}),
            )?;
            Self::app_run_show_in(tx, run_id)
        })
    }

    pub fn app_run_dispatch(&self, id: &str, current_bundle: &str) -> Result<Value> {
        self.write_tx(|conn| {

                    let tx = &mut *conn;
                    let run = Self::app_run_show_in(&tx, id)?;
                    Self::app_current_in(&tx, &run, current_bundle)?;
                    if !matches!(run["state"].as_str(), Some("approved" | "running"))
                        || run["approved_digest"] != run["snapshot_digest"]
                    {
                        return Err(Error::rejected("run execution approval is absent or stale"));
                    }
                    for assignment in run["snapshot"]["assignments"].as_object().unwrap().values() {
                        let alias = assignment["alias"].as_str().unwrap();
                        let worker = self.agent_in(&tx, alias)?;
                        if !(worker.enabled || Self::agent_auto_parked_in(&tx, &worker)?)
 || Self::app_binding_identity(&worker) != assignment["identity"] {
                            return Err(Error::rejected("registered app assignment changed"));
                        }
                    }
                    let rows=tx.query_vec("SELECT step_id,task_id,spec,identity_digest FROM app_run_steps WHERE run_id=? AND state='pending' ORDER BY step_id",[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?;
                    for (step_id, task_id, spec, generation) in rows {
                        let step: LocalStep =
                            serde_json::from_str(&spec).map_err(|e| Error::internal(e.to_string()))?;
                        let mut dependencies = Vec::new();
                        let mut ready = true;
                        for dep in &step.dependencies {
                            let state: String = tx.query_row(
                                "SELECT state FROM app_run_steps WHERE run_id=? AND step_id=?",
                                params![id, dep],
                                |r| r.get(0),
                            )?;
                            if state != "succeeded" {
                                ready = false;
                                break;
                            }
                            if let Some(artifact)=tx.query_opt("SELECT id,digest,media_type,length(content) FROM app_run_artifacts WHERE run_id=? AND step_id=?",params![id,dep],|r|Ok(json!({"artifact_id":r.get::<_,String>(0)?,"producer_step_id":dep,"revision":1,"sha256":r.get::<_,String>(1)?,"media_type":r.get::<_,String>(2)?,"size":r.get::<_,i64>(3)?})))? {dependencies.push(artifact);}
                        }
                        if !ready {
                            continue;
                        }
                        // CAD-1171: a host capability step is dispatched
                        // without a message — the daemon runs it
                        // in-process for the operator's own click.
                        if step.kind == "capability" {
                            tx.execute("UPDATE tasks SET state='dispatched',revision=1,updated=? WHERE id=? AND state='draft'",params![now(),task_id])?;
                            tx.execute("UPDATE app_run_steps SET state='dispatched' WHERE run_id=? AND step_id=? AND state='pending'",params![id,step_id])?;
                            Self::event(
                                &tx,
                                Self::DAEMON_STREAM,
                                "app_run_step_dispatched",
                                json!({"run_id":id,"step_id":step_id,"host":true}),
                            )?;
                            continue;
                        }
                        let worker = self.agent_in(&tx, &step.assignee)?;
                        let expected = &run["snapshot"]["assignments"][&step_id];
                        if material_digest(&Self::app_binding_identity(&worker)) != generation
                            || worker.role != expected["role"].as_str().unwrap()
                            || worker.provider != expected["provider"].as_str().unwrap()
                            || worker.endpoint_kind != expected["endpoint_kind"].as_str().unwrap()
                            || worker
                                .params
                                .as_ref()
                                .and_then(|p| p.get("upstream"))
                                .and_then(Value::as_str)
                                != run["snapshot"]["owner_pm"].as_str()
                        {
                            return Err(Error::rejected(
                                "registered assignment identity or group changed; create a new approved run",
                            ));
                        }
                        let message = format!("app-{id}-{step_id}-r1");
                        let envelope = json!({"schema":1,"run_id":id,"step_id":step_id,"revision":1,"kind":step.kind,"instruction":step.instruction,"dependencies":dependencies,
                            "source":run["snapshot"]["source"],
                            "capability_slots":run["snapshot"]["workflow"]["capability_slots"],
                            "required_asset_slot":run["snapshot"]["workflow"]["required_asset_slot"],
                            "result_contract":"Return exactly one complete JSON envelope as your final text, with no prose, heading, or Markdown fence. It must have schema=1, kind, run_id, step_id, revision. Producer: outcome=succeeded, artifacts=[{media_type:text/markdown,text:...}]. Reviewer: producer_step_id, producer_revision=1, artifact_sha256, decision=approve|revise, rationale. If required_asset_slot is set, approval must include asset_receipt_id and asset_sha256 from that exact slot's fetched receipt; otherwise these fields are optional for a reviewed binary asset. Fetch dependencies with cadence app run artifact using the active message/turn token. No outward effects authorized.","max_artifact_bytes":ARTIFACT_BYTES});
                        let body = envelope.to_string();
                        if body.len() > super::ENQUEUE_BYTES {
                            return Err(Error::rejected(
                                "encoded app kickoff exceeds transport byte limit",
                            ));
                        }
                        self.enqueue_tx(
                            &tx,
                            &step.assignee,
                            &body,
                            None,
                            &message,
                            "app_run_dispatch",
                            Some(&task_id),
                            None,
                            None,
                            &Sender::Unattributed,
                            super::Priority::Normal,
                            None,
                            None,
                        )?;
                        tx.execute("UPDATE tasks SET state='dispatched',revision=1,dispatch_message=?,updated=? WHERE id=? AND state='draft'",params![message,now(),task_id])?;
                        tx.execute("UPDATE app_run_steps SET state='dispatched',message_id=? WHERE run_id=? AND step_id=? AND state='pending'",params![message,id,step_id])?;
                        Self::event(
                            &tx,
                            Self::DAEMON_STREAM,
                            "app_run_step_dispatched",
                            json!({"run_id":id,"step_id":step_id,"message_id":message,"snapshot_digest":run["snapshot_digest"],"assignee":step.assignee}),
                        )?;
                    }
                    tx.execute(
                        "UPDATE app_runs SET state='running',updated=? WHERE id=?",
                        params![now(), id],
                    )?;
                    Self::app_run_show_in(&tx, id)
        })
    }
    /// Authority loss is terminal; existing artifacts and turn receipts remain
    /// immutable. This never retries uncertain provider work.
    pub fn app_run_invalidate(&self, id: &str) -> Result<()> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            self.app_run_invalidate_in(&tx, id)?;
            Ok(())
        })
    }
    pub(super) fn app_run_invalidate_in(&self, tx: &impl super::StoreConn, id: &str) -> Result<()> {
        let changed = tx.execute("UPDATE app_runs SET state='failed',approved_digest=NULL,updated=? WHERE id=? AND state IN ('awaiting_approval','approved','running')", params![now(), id])?;
        if changed != 0 {
            tx.execute(
                "UPDATE jobs SET state='failed',updated=? WHERE id=?",
                params![now(), id],
            )?;
            // Keep an already running transport's association until its
            // authenticated completion records failure. The run predicate
            // already closes material/dispatch authority; erasing the step
            // association would roll back the transport's terminal receipt.
            tx.execute("UPDATE app_run_steps SET state='failed' WHERE run_id=? AND state IN ('pending','dispatched') AND NOT EXISTS (SELECT 1 FROM messages m WHERE m.id=app_run_steps.message_id AND m.state='running')", [id])?;
            tx.execute("UPDATE tasks SET state='failed',error='app authority or assignment is no longer current',updated=? WHERE id IN (SELECT task_id FROM app_run_steps WHERE run_id=? AND state='failed')", params![now(), id])?;
            tx.execute("UPDATE messages SET state='failed',error='app submission authorization changed',completed=? WHERE source='app_run_dispatch' AND state IN ('queued','submitting') AND id IN (SELECT message_id FROM app_run_steps WHERE run_id=?)",params![now(),id])?;
            Self::event(
                tx,
                Self::DAEMON_STREAM,
                "app_run_invalidated",
                json!({"run_id":id,"reason":"authority_or_assignment_changed"}),
            )?;
        }
        Ok(())
    }
    pub fn app_run_pending(&self) -> Result<Vec<(String, String, String)>> {
        Ok(self
            .conn()
            .prepare("SELECT id,install_id,bundle_digest FROM app_runs WHERE state='running'")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn app_artifact_with_digest(
        &self,
        id: &str,
        turn: Option<(&str, &str)>,
        current_bundle: &str,
    ) -> Result<Value> {
        let turn =
            turn.ok_or_else(|| Error::rejected("worker artifact fetch requires an assigned turn"))?;
        self.app_artifact_read(id, Some(turn), Some(current_bundle))
    }
    /// Caller proof is supplied by the daemon's operator-only route. Audit
    /// access is distinct from current execution or installation authority.
    pub fn app_artifact_for_operator(&self, id: &str) -> Result<Value> {
        self.app_artifact_read(id, None, None)
    }
    fn app_artifact_read(
        &self,
        id: &str,
        turn: Option<(&str, &str)>,
        current_bundle: Option<&str>,
    ) -> Result<Value> {
        let conn = self.conn();
        let (run_id, producer_step, digest, media_type, bytes): (
            String,
            String,
            String,
            String,
            Vec<u8>,
        ) = conn
            .query_row(
                "SELECT a.run_id,a.step_id,a.digest,a.media_type,substr(a.content,1,?2) FROM app_run_artifacts a JOIN app_run_steps s ON s.run_id=a.run_id AND s.step_id=a.step_id JOIN app_runs r ON r.id=a.run_id WHERE a.id=?1",
                params![id, (ARTIFACT_BYTES + 1) as i64],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?
            .ok_or_else(|| Error::rejected("artifact unavailable"))?;
        let run = Self::app_run_show_in(&conn, &run_id)?;
        if let Some((message, token)) = turn {
            Self::app_current_in(
                &conn,
                &run,
                current_bundle.ok_or_else(|| {
                    Error::rejected("worker fetch requires current installation proof")
                })?,
            )?;
            if run["state"] != "running" || run["approved_digest"] != run["snapshot_digest"] {
                return Err(Error::rejected(
                    "artifact run execution is no longer approved",
                ));
            }
            let msg = self
                .message_in(&conn, message)?
                .filter(|m| m.state == "running" && m.turn_id.as_deref() == Some(token))
                .ok_or_else(|| Error::rejected("artifact needs an active assigned turn"))?;
            let spec:String=conn.query_row("SELECT spec FROM app_run_steps WHERE run_id=? AND message_id=? AND state='dispatched'",params![run_id,message],|r|r.get(0)).optional()?.ok_or_else(||Error::rejected("artifact is not a dependency of this turn"))?;
            let step: LocalStep =
                serde_json::from_str(&spec).map_err(|e| Error::internal(e.to_string()))?;
            let generation = self.agent_in(&conn, &msg.alias)?.generation;
            let assigned = &run["snapshot"]["assignments"][&step.id];
            if step.assignee != msg.alias
                || material_digest(&Self::app_binding_identity(
                    &self.agent_in(&conn, &msg.alias)?,
                )) != assigned["identity_digest"].as_str().unwrap()
                || !step.dependencies.contains(&producer_step)
                || !local_token_current(
                    assigned["provider"].as_str().unwrap(),
                    assigned["endpoint_kind"].as_str().unwrap(),
                    generation.as_deref(),
                    token,
                )
            {
                return Err(Error::rejected(
                    "artifact is not authorized for this assigned turn",
                ));
            }
        }
        if bytes.is_empty()
            || bytes.len() > ARTIFACT_BYTES
            || !matches!(media_type.as_str(), "text/plain" | "text/markdown")
            || artifact_digest(&bytes) != digest
        {
            return Err(Error::rejected(
                "persisted artifact integrity or bounds refused",
            ));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| Error::internal("stored text artifact is not UTF-8"))?;
        Ok(json!({"id":id,"digest":digest,"media_type":media_type,"size":text.len(),"text":text}))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TextArtifact {
    media_type: String,
    text: String,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum LocalResult {
    #[serde(rename = "produce_text")]
    Produce {
        schema: u32,
        run_id: String,
        step_id: String,
        revision: u32,
        outcome: String,
        artifacts: Vec<TextArtifact>,
    },
    #[serde(rename = "review_text")]
    Review {
        schema: u32,
        run_id: String,
        step_id: String,
        revision: u32,
        producer_step_id: String,
        producer_revision: u32,
        artifact_sha256: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        asset_receipt_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        asset_sha256: Option<String>,
        decision: String,
        rationale: String,
    },
}

/// Bound applied before either parse path below. A max-size artifact can
/// expand when JSON-escaped, so the raw final text may legitimately exceed
/// `ARTIFACT_BYTES` by several times; anything beyond this still fails closed.
pub(super) const MAX_RESULT_TEXT_BYTES: usize = ARTIFACT_BYTES * 6 + 64 * 1024;

/// Locate exactly one top-level balanced-brace object in `text`, honouring
/// JSON string escapes. Returns `None` for zero, multiple, or unbalanced
/// brace spans, or when a fence marker is present anywhere outside the span.
fn extract_single_json_object(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut depth = 0usize;
    let mut start: Option<usize> = None;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                if depth == 0 {
                    spans.push((start.take().expect("brace span start"), i + 1));
                }
            }
            _ => {}
        }
    }
    if in_string || depth != 0 || spans.len() != 1 {
        return None;
    }
    let (start, end) = spans[0];
    if text[..start].contains("```") || text[end..].contains("```") {
        return None;
    }
    Some(&text[start..end])
}

/// Pi sometimes surrounds its final material envelope with explanatory
/// prose, either bare or inside one standalone `json` fence. Accept exactly
/// one complete, bounded envelope in either form amid brace-free prose;
/// never search prose for the first of several parseable objects. Multiple,
/// conflicting, malformed, oversized, or extra-fence candidates fail closed.
/// The decoded result still passes the active-turn, pinned-run, artifact and
/// review checks below; unknown fields and forged identity are refused by
/// the strict `LocalResult` deserialization.
pub(super) fn parse_local_result_text(text: &str) -> Option<LocalResult> {
    if text.len() > MAX_RESULT_TEXT_BYTES {
        return None;
    }
    if text.contains("```") {
        let mut opening = None;
        let mut closing = None;
        let mut offset = 0;
        for line in text.split_inclusive('\n') {
            let marker = line.trim_end_matches(['\r', '\n']);
            match marker {
                "```json" if opening.is_none() && closing.is_none() => {
                    opening = Some((offset, offset + line.len()));
                }
                "```" if opening.is_some() && closing.is_none() => {
                    closing = Some((offset, offset + line.len()));
                }
                _ => {}
            }
            offset += line.len();
        }
        let ((open_start, body_start), (body_end, close_end)) = (opening?, closing?);
        let before = &text[..open_start];
        let after = &text[close_end..];
        if [before, after]
            .iter()
            .any(|part| part.contains("```") || part.contains('{') || part.contains('}'))
        {
            return None;
        }
        return serde_json::from_str::<LocalResult>(&text[body_start..body_end]).ok();
    }
    if let Ok(result) = serde_json::from_str::<LocalResult>(text) {
        return Some(result);
    }
    serde_json::from_str::<LocalResult>(extract_single_json_object(text)?).ok()
}

impl Store {
    /// Material transitions occur only on the actual active app kickoff. This
    /// transaction records durable eligibility; it never acquires the PM lock.
    pub(super) fn app_run_finished_in(
        &self,
        tx: &impl super::StoreConn,
        message: &Message,
        status: &str,
        result: &Value,
        proof: &AppCompletionProof,
    ) -> Result<bool> {
        let Some(task_id) = message.task_id.as_deref() else {
            return Ok(false);
        };
        let Some((run_id,step_id,spec,generation,state,message_id))=tx.query_row("SELECT run_id,step_id,spec,identity_digest,state,message_id FROM app_run_steps WHERE task_id=?",[task_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,Option<String>>(5)?))).optional()? else{return Ok(false)};
        if matches!(proof, AppCompletionProof::OperatorReconcile) {
            return self
                .app_step_failed_in(
                    tx,
                    &run_id,
                    &step_id,
                    task_id,
                    "operator reconciliation preserves transport history, not material success",
                )
                .map(|_| true);
        }
        // Reconciliation is transport history only, never material authority.
        if state != "dispatched"
            || message_id.as_deref() != Some(&message.id)
            || message.source != "app_run_dispatch"
        {
            return Err(Error::rejected("stale app completion association"));
        }
        let current = self
            .message_in(tx, &message.id)?
            .ok_or_else(|| Error::rejected("app kickoff disappeared"))?;
        let step: LocalStep =
            serde_json::from_str(&spec).map_err(|e| Error::internal(e.to_string()))?;
        let worker = self.agent_in(tx, &message.alias)?;
        let task = self.task_in(tx, task_id)?;
        if status != "completed" {
            return self
                .app_step_failed_in(
                    tx,
                    &run_id,
                    &step_id,
                    task_id,
                    "app turn did not produce successful material",
                )
                .map(|_| true);
        }
        let AppCompletionProof::ActiveEndpoint { turn_id } = proof else {
            return self
                .app_step_failed_in(
                    tx,
                    &run_id,
                    &step_id,
                    task_id,
                    "material completion lacks a proven active endpoint turn",
                )
                .map(|_| true);
        };
        let token = current
            .turn_id
            .as_deref()
            .ok_or_else(|| Error::rejected("app material result needs its active turn"))?;
        if turn_id != token
            || result.get("turn_id").and_then(Value::as_str) != Some(token)
            || material_digest(&Self::app_binding_identity(&worker)) != generation
            || message.alias != step.assignee
            || task.assignee.as_deref() != Some(&step.assignee)
            || task.revision != 1
            || task.dispatch_message.as_deref() != Some(&message.id)
            || !local_token_current(
                &worker.provider,
                &worker.endpoint_kind,
                worker.generation.as_deref(),
                token,
            )
        {
            return Err(Error::rejected(
                "app material result assignment or turn is stale",
            ));
        }
        // finish_in has already changed the message terminal state, but its
        // durable turn and association are still authoritative in this tx.
        let run = Self::app_run_show_in(tx, &run_id)?;
        let expected = &run["snapshot"]["assignments"][&step_id];
        let identity_matches = worker.role == expected["role"].as_str().unwrap()
            && worker.provider == expected["provider"].as_str().unwrap()
            && worker.endpoint_kind == expected["endpoint_kind"].as_str().unwrap()
            && worker
                .params
                .as_ref()
                .and_then(|p| p.get("upstream"))
                .and_then(Value::as_str)
                == run["snapshot"]["owner_pm"].as_str();
        if status != "completed"
            || run["state"] != "running"
            || run["approved_digest"] != run["snapshot_digest"]
            || !identity_matches
            || Self::app_current_in(tx, &run, run["snapshot"]["bundle_digest"].as_str().unwrap())
                .is_err()
        {
            return self
                .app_step_failed_in(
                    tx,
                    &run_id,
                    &step_id,
                    task_id,
                    "provider did not return a successful current run result",
                )
                .map(|_| true);
        };
        let decoded = if let Some(text) = result.get("text").and_then(Value::as_str) {
            parse_local_result_text(text)
        } else {
            serde_json::from_value::<LocalResult>(result.clone()).ok()
        };
        let Some(decoded) = decoded else {
            return self
                .app_step_failed_in(
                    tx,
                    &run_id,
                    &step_id,
                    task_id,
                    "invalid local result envelope",
                )
                .map(|_| true);
        };
        let normalized_result =
            serde_json::to_value(&decoded).map_err(|e| Error::internal(e.to_string()))?;
        let mut valid = false;
        match decoded {
            LocalResult::Produce {
                schema,
                run_id: r,
                step_id: s,
                revision,
                outcome,
                artifacts,
            } => {
                if schema == 1
                    && r == run_id
                    && s == step_id
                    && revision == 1
                    && step.kind == "produce_text"
                    && outcome == "succeeded"
                    && artifacts.len() == 1
                {
                    let artifact = &artifacts[0];
                    let total:i64=tx.query_row("SELECT COALESCE(SUM(length(content)),0) FROM app_run_artifacts WHERE run_id=?",[&run_id],|r|r.get(0))?;
                    if matches!(artifact.media_type.as_str(), "text/plain" | "text/markdown")
                        && !artifact.text.is_empty()
                        && artifact.text.len() <= ARTIFACT_BYTES
                        && total as usize + artifact.text.len() <= RUN_ARTIFACT_BYTES
                        && carry_text_bytes_ok(&run, &artifact.text)
                    {
                        let id = format!("artifact-{run_id}-{step_id}");
                        let digest = artifact_digest(artifact.text.as_bytes());
                        tx.execute(
                            "INSERT INTO app_run_artifacts VALUES(?,?,?,?,?,?,?,?,?,?)",
                            params![
                                id,
                                run_id,
                                step_id,
                                message.id,
                                token,
                                message.alias,
                                digest,
                                artifact.media_type,
                                artifact.text.as_bytes(),
                                now()
                            ],
                        )?;
                        Self::event(
                            tx,
                            Self::DAEMON_STREAM,
                            "app_run_artifact_recorded",
                            json!({"run_id":run_id,"step_id":step_id,"artifact_id":id,"sha256":digest,"producer":message.alias}),
                        )?;
                        valid = true;
                    }
                }
            }
            LocalResult::Review {
                schema,
                run_id: r,
                step_id: s,
                revision,
                producer_step_id,
                producer_revision,
                artifact_sha256,
                asset_receipt_id,
                asset_sha256,
                decision,
                rationale,
            } => {
                if schema == 1
                    && r == run_id
                    && s == step_id
                    && revision == 1
                    && producer_revision == 1
                    && step.kind == "review_text"
                    && step.dependencies == vec![producer_step_id.clone()]
                    && !rationale.trim().is_empty()
                    && rationale.len() <= 16 * 1024
                    && matches!(decision.as_str(), "approve" | "revise")
                {
                    if let Some((id,digest,producer))=tx.query_row("SELECT id,digest,producer FROM app_run_artifacts WHERE run_id=? AND step_id=?",params![run_id,producer_step_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional()? {
                        let required_asset_slot = run["snapshot"]["workflow"]["required_asset_slot"].as_str();
                        let asset_valid = match (&asset_receipt_id,&asset_sha256) {
                            (None,None) => {
                                // An image carry always needs its asset pin —
                                // no bypass through a missing required_asset_slot
                                // declaration; a carries entry alone never suffices.
                                decision=="revise"
                                    || (required_asset_slot.is_none()
                                        && run["snapshot"]["carry"]["retain"].as_str() != Some("image"))
                            }
                            (Some(receipt_id),Some(asset_digest)) if decision=="approve" => {
                                if run["snapshot"]["carry"]["retain"].as_str() == Some("image") {
                                    // carried asset: must equal the frozen record with its
                                    // source chain re-verified — never a fresh receipt
                                    // relabeled, never another run's bytes.
                                    let carry = &run["snapshot"]["carry"];
                                    json!(receipt_id) == carry["asset_receipt_id"]
                                        && json!(asset_digest) == carry["asset_digest"]
                                        && super::app_capabilities::asset_material_in(tx,receipt_id)
                                            .is_ok_and(|(receipt,_)| receipt["receipt_schema"]==2
                                                && receipt["run_id"]==carry["from_run_id"]
                                                && receipt["step_id"]==carry["asset_producer_step"].as_str().unwrap_or_default()
                                                && receipt["slot"]==carry["asset_slot"]
                                                && receipt["asset"]["digest"]==carry["asset_digest"]
                                                && receipt["binding_digest"]==carry["asset_binding_digest"])
                                } else {
                                super::app_capabilities::asset_material_in(tx,receipt_id)
                                    .is_ok_and(|(receipt,_)| receipt["receipt_schema"]==2
                                        && receipt["run_id"]==run_id
                                        && receipt["step_id"]==producer_step_id
                                        && required_asset_slot.is_none_or(|slot| receipt["slot"]==slot)
                                        && receipt["asset"]["digest"]==*asset_digest
                                        && run["snapshot"]["capabilities"][receipt["slot"].as_str().unwrap_or("")]["digest"]==receipt["binding_digest"])
                                }
                            },
                            _ => false,
                        };
                        if digest==artifact_sha256 && producer!=message.alias && asset_valid {
                            tx.execute("INSERT INTO app_run_reviews(run_id,step_id,artifact_id,artifact_digest,reviewer,message_id,decision,rationale,asset_receipt_id,asset_digest) VALUES(?,?,?,?,?,?,?,?,?,?)",params![run_id,step_id,id,digest,message.alias,message.id,decision,rationale,asset_receipt_id,asset_sha256])?;
                            Self::event(tx,Self::DAEMON_STREAM,"app_run_step_reviewed",json!({"run_id":run_id,"step_id":step_id,"artifact_digest":digest,"reviewer":message.alias,"decision":decision}))?;
                            valid=decision=="approve";
                        }
                    }
                }
            }
        }
        if !valid {
            return self
                .app_step_failed_in(
                    tx,
                    &run_id,
                    &step_id,
                    task_id,
                    "material output or pinned review refused",
                )
                .map(|_| true);
        };
        let result_digest = material_digest(
            &json!({"material":normalized_result,"producer":message.alias,"message":message.id}),
        );
        tx.execute("UPDATE app_run_steps SET state='succeeded',result_digest=? WHERE run_id=? AND step_id=?",params![result_digest,run_id,step_id])?;
        tx.execute(
            "UPDATE tasks SET state='done',updated=? WHERE id=?",
            params![now(), task_id],
        )?;
        let pending: i64 = tx.query_row(
            "SELECT COUNT(*) FROM app_run_steps WHERE run_id=? AND state!='succeeded'",
            [&run_id],
            |r| r.get(0),
        )?;
        if pending == 0 {
            tx.execute(
                "UPDATE app_runs SET state='succeeded',updated=? WHERE id=?",
                params![now(), run_id],
            )?;
            tx.execute(
                "UPDATE jobs SET state='done',updated=? WHERE id=?",
                params![now(), run_id],
            )?;
        }
        Self::event(
            tx,
            Self::DAEMON_STREAM,
            if pending == 0 {
                "app_run_completed"
            } else {
                "app_run_step_result_recorded"
            },
            json!({"run_id":run_id,"step_id":step_id,"result_digest":result_digest,"producer":message.alias,"eligible_successors":pending>0}),
        )?;
        Ok(true)
    }
    fn app_step_failed_in(
        &self,
        tx: &impl super::StoreConn,
        run: &str,
        step: &str,
        task: &str,
        reason: &str,
    ) -> Result<()> {
        tx.execute(
            "UPDATE app_run_steps SET state='failed' WHERE run_id=? AND step_id=?",
            params![run, step],
        )?;
        tx.execute(
            "UPDATE tasks SET state='failed',error=?,updated=? WHERE id=?",
            params![reason, now(), task],
        )?;
        tx.execute(
            "UPDATE app_runs SET state='failed',updated=? WHERE id=? AND state!='cancelled'",
            params![now(), run],
        )?;
        tx.execute(
            "UPDATE jobs SET state='failed',updated=? WHERE id=? AND state!='cancelled'",
            params![now(), run],
        )?;
        Self::event(
            tx,
            Self::DAEMON_STREAM,
            "app_run_failed",
            json!({"run_id":run,"step_id":step,"reason":reason}),
        )?;
        Ok(())
    }
}

impl Store {
    pub fn app_artifact_run_id(&self, id: &str) -> Result<String> {
        self.conn()
            .query_row(
                "SELECT run_id FROM app_run_artifacts WHERE id=?",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| Error::rejected("artifact unavailable"))
    }
    pub fn app_artifact_installation(&self, id: &str) -> Result<String> {
        self.conn().query_row("SELECT r.install_id FROM app_run_artifacts a JOIN app_runs r ON r.id=a.run_id WHERE a.id=?",[id],|r|r.get(0)).optional()?.ok_or_else(||Error::rejected("artifact unavailable"))
    }
    pub fn app_task_owned(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn()
            .query_row("SELECT 1 FROM app_run_steps WHERE task_id=?", [id], |_| {
                Ok(())
            })
            .optional()?
            .is_some())
    }
}

/// Whether a worker turn token is still current for its provider+endpoint.
/// Shared by the ordinary capability-turn path and the carry-read branch;
/// widening visibility changes no behavior.
pub(super) fn local_token_current(
    provider: &str,
    kind: &str,
    generation: Option<&str>,
    token: &str,
) -> bool {
    let Some(spec) = crate::adapter::registry::spec_opt(provider, kind) else {
        return false;
    };
    !token.is_empty()
        && (spec.turn_token.is_none()
            || crate::adapter::registry::turn_token_current(provider, kind, generation, token))
}

pub struct LocalRunRequest<'a> {
    pub install_id: &'a str,
    pub bundle_digest: &'a str,
    pub workflow: &'a LocalWorkflow,
    pub inputs: &'a BTreeMap<String, String>,
    pub request_id: &'a str,
    /// CAD-1171: required for an agent run; absent (None) for a host run.
    pub owner_pm: Option<&'a str>,
    pub project_link: Option<&'a str>,
}

impl Store {
    /// The socket caller and live turn select the run; a request cannot name
    /// another run, installation, context, account, or provider.
    pub(crate) fn app_capability_turn(
        &self,
        alias: &str,
        message: &str,
        token: &str,
        slot: &str,
        bundle: &str,
    ) -> Result<(Value, String, super::app_bindings::BindingProof)> {
        let conn = self.conn();
        let msg = self
            .message_in(&conn, message)?
            .filter(|m| {
                m.alias == alias
                    && m.source == "app_run_dispatch"
                    && m.state == "running"
                    && m.turn_id.as_deref() == Some(token)
            })
            .ok_or_else(|| Error::rejected("capability needs the active assigned app turn"))?;
        self.app_message_admit_in(&conn, &msg, bundle)?;
        let (run_id, step_id, spec): (String, String, String) = conn.query_row(
            "SELECT run_id,step_id,spec FROM app_run_steps WHERE message_id=? AND state='dispatched'",
            [message], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        )?;
        let step: LocalStep = serde_json::from_str(&spec)?;
        let run = Self::app_run_show_in(&conn, &run_id)?;
        let assigned = &run["snapshot"]["assignments"][&step_id];
        let generation = self.agent_in(&conn, alias)?.generation;
        if step.assignee != alias
            || !local_token_current(
                assigned["provider"].as_str().unwrap_or_default(),
                assigned["endpoint_kind"].as_str().unwrap_or_default(),
                generation.as_deref(),
                token,
            )
        {
            return Err(Error::rejected(
                "capability turn endpoint is no longer current",
            ));
        }
        if run["snapshot"]["schema"] != 4
            || !run["snapshot"]["workflow"]["capability_slots"]
                .as_array()
                .is_some_and(|slots| slots.iter().any(|value| value == slot))
        {
            return Err(Error::rejected(
                "slot is not in the frozen run capability set",
            ));
        }
        let proof: super::app_bindings::BindingProof =
            serde_json::from_value(run["snapshot"]["capabilities"][slot].clone())
                .map_err(|_| Error::rejected("frozen capability binding is absent"))?;
        if !matches!(
            proof.config["mapping"]["effect"].as_str(),
            Some("read" | "draft")
        ) {
            return Err(Error::rejected(
                "app capability effects are read or draft only",
            ));
        }
        Ok((run, step_id, proof))
    }

    pub fn app_message_installation(&self, message: &str) -> Result<Option<(String, String)>> {
        {
            self.read_tx(|conn| {

                            Ok(conn.query_opt("SELECT r.id,r.install_id FROM app_run_steps s JOIN app_runs r ON r.id=s.run_id WHERE s.message_id=?",[message],|r|Ok((r.get(0)?,r.get(1)?)))?)
            })
        }
    }
    pub(super) fn app_message_admit_in(
        &self,
        conn: &impl super::StoreConn,
        message: &Message,
        bundle: &str,
    ) -> Result<()> {
        let (run_id,spec,generation):(String,String,String)=conn.query_row("SELECT run_id,spec,identity_digest FROM app_run_steps WHERE message_id=? AND state='dispatched'",[&message.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.ok_or_else(||Error::rejected("app message is not a current dispatched step"))?;
        let run = Self::app_run_show_in(conn, &run_id)?;
        Self::app_current_in(conn, &run, bundle)?;
        if run["state"] != "running" || run["approved_digest"] != run["snapshot_digest"] {
            return Err(Error::rejected("app message lacks current run approval"));
        }
        let step: LocalStep =
            serde_json::from_str(&spec).map_err(|e| Error::internal(e.to_string()))?;
        let actual = self.agent_in(conn, &step.assignee)?;
        let pinned = &run["snapshot"]["assignments"][&step.id];
        if !actual.enabled
            || actual.alias != message.alias
            || material_digest(&Self::app_binding_identity(&actual)) != generation
            || actual.role != pinned["role"].as_str().unwrap()
            || actual.provider != pinned["provider"].as_str().unwrap()
            || actual.endpoint_kind != pinned["endpoint_kind"].as_str().unwrap()
            || actual
                .params
                .as_ref()
                .and_then(|p| p.get("upstream"))
                .and_then(Value::as_str)
                != run["snapshot"]["owner_pm"].as_str()
        {
            return Err(Error::rejected(
                "app message assigned endpoint identity changed",
            ));
        }
        Ok(())
    }
    pub fn app_message_admit(&self, message: &Message, bundle: &str) -> Result<()> {
        self.read_tx(|conn| self.app_message_admit_in(&conn, message, bundle))
    }
    pub fn reject_app_submission(&self, message: &str) -> Result<()> {
        self.write_tx(|conn| {

                    let tx = &mut *conn;
                    if let Some((run, step, task)) = tx
                        .query_opt(
                            "SELECT run_id,step_id,task_id FROM app_run_steps WHERE message_id=?",
                            [message],
                            |r| {
                                Ok((
                                    r.get::<_, String>(0)?,
                                    r.get::<_, String>(1)?,
                                    r.get::<_, String>(2)?,
                                ))
                            },
                        )?
                    {
                        let changed=tx.execute("UPDATE messages SET state='failed',error='app submission authorization changed',completed=? WHERE id=? AND state IN ('queued','submitting')",params![now(),message])?;
                        if changed > 0 {
                            self.app_step_failed_in(
                                &tx,
                                &run,
                                &step,
                                &task,
                                "app submission authorization changed",
                            )?;
                        }
                    }
                    Ok(())
        })
    }
}

impl LocalWorkflow {
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
impl Store {
    /// Called while holding the PM lock: admission cannot race another run
    /// creation. Terminal history stays in SQLite; no active turn is retired.
    pub fn app_install_upgrade_ready(&self, install: &str) -> Result<()> {
        let conn = self.conn();
        let unresolved_effects: i64 = conn.query_row(
            "SELECT count(*) FROM app_effect_authorizations a JOIN platform_effects e ON e.effect_id=a.effect_id WHERE a.install_id=? AND e.state IN ('waiting','decided','executing','reconcile')",
            [install],
            |row| row.get(0),
        )?;
        if unresolved_effects != 0 {
            return Err(Error::rejected(
                "installation has an unresolved app effect; decide or reconcile it before upgrade",
            ));
        }
        let active: i64 = conn.query_row(
            "SELECT count(*) FROM app_runs WHERE install_id=? AND state IN ('awaiting_approval','approved','running')",
            [install],
            |row| row.get(0),
        )?;
        if active != 0 {
            return Err(Error::rejected(
                "installation has a nonterminal run; finish or explicitly cancel it before upgrade",
            ));
        }
        let grants: i64 = conn.query_row(
            "SELECT count(*) FROM app_grants WHERE install_id=?",
            [install],
            |row| row.get(0),
        )?;
        if grants != 0 {
            return Err(Error::rejected(
                "installation has derived grants; revoke them before upgrade",
            ));
        }
        Ok(())
    }

    pub fn app_capability_status(&self, id: &str, digest: &str) -> Result<Value> {
        Ok(self
            .conn()
            .query_row(
                "SELECT epoch,state FROM app_install_capabilities WHERE install_id=? AND digest=?",
                params![id, digest],
                |r| Ok(json!({"epoch":r.get::<_,i64>(0)?,"state":r.get::<_,String>(1)?})),
            )
            .optional()?
            .unwrap_or(json!({"state":"unapproved"})))
    }
}

pub(super) enum AppCompletionProof {
    ActiveEndpoint { turn_id: String },
    Unproven,
    OperatorReconcile,
}
impl Store {
    pub(super) fn app_completion_proof(
        &self,
        conn: &impl super::StoreConn,
        message: &Message,
        result: &Value,
    ) -> Result<AppCompletionProof> {
        let current = self.message_in(conn, &message.id)?;
        if let Some(current) = current.filter(|m| {
            m.source == "app_run_dispatch" && m.state == "running" && m.alias == message.alias
        }) {
            if let Some(turn) = current
                .turn_id
                .filter(|turn| result.get("turn_id").and_then(Value::as_str) == Some(turn.as_str()))
            {
                return Ok(AppCompletionProof::ActiveEndpoint { turn_id: turn });
            }
        }
        Ok(AppCompletionProof::Unproven)
    }
}

impl Store {
    /// Transcripts can retain app material after a turn ends. Never infer
    /// public readability merely from an idle endpoint.
    pub(crate) fn app_material_endpoint(&self, alias: &str) -> Result<bool> {
        Ok(self.conn().query_row("SELECT 1 FROM app_run_steps s JOIN tasks t ON t.id=s.task_id WHERE t.assignee=? LIMIT 1", [alias], |_| Ok(())).optional()?.is_some())
    }
    pub(crate) fn app_effect_guard(&self, alias: &str, task: Option<&str>) -> Result<()> {
        if self
            .running_messages(alias)?
            .iter()
            .any(|m| m.source == "app_run_dispatch")
            || task
                .map(|id| self.app_task_owned(id))
                .transpose()?
                .unwrap_or(false)
        {
            return Err(Error::rejected("local app runs authorize run-owned text artifacts only; account grants do not authorize app outward effects"));
        }
        Ok(())
    }
}
