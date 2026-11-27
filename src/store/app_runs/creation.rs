//! Validate app-run creation inputs and persist the frozen snapshot and initial rows.

use rusqlite::params;
use serde_json::{json, Value};
use std::collections::BTreeMap;

use super::{
    human_receipt, kickoff_body, material_digest, LocalRunProvenance, LocalRunRequest,
    ARTIFACT_BYTES, KICKOFF_DEPENDENCY_BYTES, RUN_ARTIFACT_BYTES,
};
use crate::error::{Error, Result};
use crate::issue::workflow;
use crate::store::{
    app_bindings, app_capabilities, app_contexts, now, Store, StoreConn, ENQUEUE_BYTES,
};

impl Store {
    pub fn app_run_create(&self, request: LocalRunRequest<'_>) -> Result<Value> {
        self.app_run_create_with_context(request, None)
    }
    pub fn app_run_create_with_context(
        &self,
        request: LocalRunRequest<'_>,
        context: Option<&app_contexts::ContextProof>,
    ) -> Result<Value> {
        self.app_run_create_with_publication(request, context, None)
    }
    pub fn app_run_create_with_publication(
        &self,
        request: LocalRunRequest<'_>,
        context: Option<&app_contexts::ContextProof>,
        binding: Option<&app_bindings::BindingProof>,
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
                team_roles: &BTreeMap::new(),
                human: None,
            },
        )
    }
    /// Internal legacy callers keep this signature: the runs it
    /// records carry NO `creator` member, which reads as unknown
    /// attribution — never a claimed operator. Only the native daemon
    /// creation path supplies a daemon-verified creator, through
    /// `app_run_create_attributed`.
    pub fn app_run_create_with_capabilities(
        &self,
        request: LocalRunRequest<'_>,
        context: Option<&app_contexts::ContextProof>,
        binding: Option<&app_bindings::BindingProof>,
        capabilities: &BTreeMap<String, app_bindings::BindingProof>,
        quotes: &BTreeMap<String, crate::platform::AppCapabilityQuote>,
        provenance: LocalRunProvenance<'_>,
    ) -> Result<Value> {
        self.app_run_create_attributed(
            request,
            context,
            binding,
            capabilities,
            quotes,
            provenance,
            None,
        )
    }
    /// CAD-1365: the creator-bearing creation path. `creator` is the
    /// daemon-verified actor json (`{kind:"operator"}` or
    /// `{kind:"user",id:<sub>}`) frozen INSIDE the snapshot before
    /// `material_digest`, so the digest covers it and it can never be
    /// mutated after creation. `None` leaves the member absent — the
    /// honest unknown attribution of the internal legacy callers.
    /// There is no creator setter: the only way a creator lands is a
    /// verified argument at creation.
    #[allow(clippy::too_many_arguments)]
    #[doc(hidden)]
    pub fn app_run_create_attributed(
        &self,
        request: LocalRunRequest<'_>,
        context: Option<&app_contexts::ContextProof>,
        binding: Option<&app_bindings::BindingProof>,
        capabilities: &BTreeMap<String, app_bindings::BindingProof>,
        quotes: &BTreeMap<String, crate::platform::AppCapabilityQuote>,
        provenance: LocalRunProvenance<'_>,
        creator: Option<&Value>,
    ) -> Result<Value> {
        let LocalRunProvenance {
            selected_source,
            input_origins,
            team_roles,
            human,
        } = provenance;
        let operation = uuid::Uuid::new_v4().to_string();
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
                            || !app_bindings::binding_current_in(
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
                            || !app_bindings::binding_current_in(
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
                        app_capabilities::source_receipt_recoverable_in(
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
                    let mut snapshot = json!({"schema":1,"install_id":install_id,"bundle_digest":bundle_digest,"epoch":epoch,"workflow":workflow,"inputs":inputs,"assignments":assignments,"owner_pm":owner_pm,"project_link":project_link,"artifact_policy":{"types":["text/plain","text/markdown"],"max_bytes":ARTIFACT_BYTES,"aggregate_bytes":RUN_ARTIFACT_BYTES}});
                    if !input_origins.is_empty() {
                        snapshot["input_origins"] = json!(input_origins);
                    }
                    // CAD-622: the team-role bindings this run froze stay
                    // in the snapshot as their own member, so a later team
                    // edit cannot drop a historical member and a content
                    // input can never be mistaken for a role name. Written
                    // even when empty: a new snapshot must never be read as
                    // a legacy one whose role keys were not recorded.
                    snapshot["team_roles"] = json!(team_roles);
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
                    // CAD-1365: freeze the verified creator inside
                    // the snapshot BEFORE the digest covers it —
                    // attribution, never authorization.
                    if let Some(creator) = creator {
                        snapshot["creator"] = creator.clone();
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
                    // CAD-1867: the kickoff envelope a step will be dispatched
                    // with is measured NOW — frozen skill material included —
                    // before any row exists. A run whose material cannot reach
                    // its worker is refused here, not half-created and then
                    // un-dispatchable. A capability step enqueues no message,
                    // so it has no envelope to fit.
                    for step in &workflow.steps {
                        if step.kind == "capability" {
                            continue;
                        }
                        let bytes = kickoff_body(&id, step, &[], &snapshot)?.len()
                            + step.dependencies.len() * KICKOFF_DEPENDENCY_BYTES;
                        if bytes > ENQUEUE_BYTES {
                            return Err(Error::rejected(format!(
                                "encoded app kickoff for step {} exceeds the transport byte \
                                 limit ({bytes} > {}) — reduce the skill package or the step \
                                 instruction",
                                step.id,
                                ENQUEUE_BYTES
                            )));
                        }
                    }
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
                        human_receipt(json!({"run_id":id,"install_id":install_id,"snapshot_digest":digest,"actor":"operator"}), human, &operation, "app_run_created", &format!("app-run:{id}"), "created"),
                    )?;
                    Self::app_run_show_in(&*tx, &id)
        })
    }
}
