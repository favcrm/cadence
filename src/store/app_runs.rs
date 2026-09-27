//! Project-free, broker-local text runs. This is not provider tool confinement.
use super::*;
use crate::issue::{parse, plan, workflow};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const ARTIFACT_BYTES: usize = 256 * 1024;
pub const RUN_ARTIFACT_BYTES: usize = 1024 * 1024;

pub fn artifact_digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
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
    pub title: String,
    pub steps: Vec<LocalStep>,
}
impl LocalWorkflow {
    pub fn parse(text: &str, inputs: &BTreeMap<String, String>) -> Result<Self> {
        let rendered = workflow::render(text, inputs)?;
        let parsed = plan::parse_plan(&rendered)?;
        let (_, body) =
            parse::split_front(&rendered).map_err(|e| Error::rejected(e.to_string()))?;
        let metadata = workflow::ticket_meta(body)?;
        if parsed.tickets.len() > 16 {
            return Err(Error::rejected("local runs support at most 16 steps"));
        }
        let mut steps = Vec::new();
        for (n, ticket) in parsed.tickets.iter().enumerate() {
            let meta: BTreeMap<_, _> = metadata[n].iter().cloned().collect();
            if meta.contains_key("tries")
                || meta.contains_key("reviewer")
                || meta.contains_key("uses")
            {
                return Err(Error::rejected("local runs require explicit review steps and support one attempt; reviewer/tries directives are unsupported"));
            }
            let kind = match meta.get("action").map(String::as_str) {
                Some("local.text.produce") => "produce_text",
                Some("local.text.review") => "review_text",
                _ => return Err(Error::rejected("unsupported execution capability: declare action: local.text.produce or local.text.review")),
            };
            let assignee = ticket.agent.clone().ok_or_else(|| {
                Error::rejected("every local step needs an explicit assigned agent")
            })?;
            crate::proto::identifier(&assignee, "local step agent")?;
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
        Ok(Self {
            title: parsed.title,
            steps,
        })
    }
}

pub(super) const SCHEMA: &str = "
CREATE TABLE app_install_capabilities(
 install_id TEXT PRIMARY KEY, epoch INTEGER NOT NULL CHECK(epoch>0),
 digest TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('approved','revoked')),
 created REAL NOT NULL);
CREATE TABLE app_runs(
 id TEXT PRIMARY KEY, install_id TEXT NOT NULL, epoch INTEGER NOT NULL,
 bundle_digest TEXT NOT NULL, snapshot TEXT NOT NULL, snapshot_digest TEXT NOT NULL,
 project_link TEXT, owner_pm TEXT NOT NULL, request_id TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('awaiting_approval','approved','running','succeeded','failed','cancelled')),
 approved_digest TEXT, created REAL NOT NULL, updated REAL NOT NULL,
 UNIQUE(install_id,request_id));
CREATE TABLE app_run_steps(
 run_id TEXT NOT NULL REFERENCES app_runs(id), step_id TEXT NOT NULL,
 task_id TEXT NOT NULL UNIQUE REFERENCES tasks(id), spec TEXT NOT NULL,
 generation TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('pending','dispatched','succeeded','failed')),
 message_id TEXT UNIQUE, result_digest TEXT,
 PRIMARY KEY(run_id,step_id));
CREATE TABLE app_run_artifacts(
 id TEXT PRIMARY KEY, run_id TEXT NOT NULL, step_id TEXT NOT NULL,
 message_id TEXT NOT NULL UNIQUE, turn_id TEXT NOT NULL, producer TEXT NOT NULL,
 digest TEXT NOT NULL, media_type TEXT NOT NULL, content BLOB NOT NULL,
 created REAL NOT NULL, UNIQUE(run_id,step_id),
 FOREIGN KEY(run_id,step_id) REFERENCES app_run_steps(run_id,step_id));
CREATE TABLE app_run_reviews(
 run_id TEXT NOT NULL, step_id TEXT NOT NULL, artifact_id TEXT NOT NULL REFERENCES app_run_artifacts(id),
 artifact_digest TEXT NOT NULL, reviewer TEXT NOT NULL, message_id TEXT UNIQUE NOT NULL,
 decision TEXT NOT NULL CHECK(decision IN ('approve','revise')), rationale TEXT NOT NULL,
 PRIMARY KEY(run_id,step_id), FOREIGN KEY(run_id,step_id) REFERENCES app_run_steps(run_id,step_id));
";

impl Store {
    pub(super) fn refuse_app_task(conn: &Connection, task_id: &str) -> Result<()> {
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
    pub(super) fn refuse_app_job(conn: &Connection, job_id: &str) -> Result<()> {
        if conn.query_row("SELECT 1 FROM app_run_steps s JOIN tasks t ON t.id=s.task_id WHERE t.job_id=? LIMIT 1",[job_id], |_| Ok(())).optional()?.is_some() {
            return Err(Error::rejected("app-owned jobs use the app run lifecycle"));
        }
        Ok(())
    }
    pub fn app_capability_decide(&self, id: &str, digest: &str, approve: bool) -> Result<Value> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        tx.execute("INSERT INTO app_install_capabilities VALUES(?,1,?,?,?) ON CONFLICT(install_id) DO UPDATE SET epoch=epoch+1,digest=excluded.digest,state=excluded.state,created=excluded.created",params![id,digest,if approve{"approved"}else{"revoked"},now()])?;
        let epoch: i64 = tx.query_row(
            "SELECT epoch FROM app_install_capabilities WHERE install_id=?",
            [id],
            |r| r.get(0),
        )?;
        Self::event(
            &tx,
            Self::DAEMON_STREAM,
            if approve {
                "app_install_capability_approved"
            } else {
                "app_install_capability_revoked"
            },
            json!({"install_id":id,"digest":digest,"epoch":epoch,"actor":"operator"}),
        )?;
        tx.commit()?;
        Ok(
            json!({"install_id":id,"epoch":epoch,"digest":digest,"approved":approve,"capabilities":["local.text.produce","local.text.review"],"outward_release":false}),
        )
    }
}

impl Store {
    pub fn app_run_create(
        &self,
        install_id: &str,
        bundle_digest: &str,
        workflow: &LocalWorkflow,
        inputs: &BTreeMap<String, String>,
        request_id: &str,
        owner_pm: &str,
        project_link: Option<&str>,
    ) -> Result<Value> {
        crate::proto::identifier(request_id, "app run request ID")?;
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let epoch:i64=tx.query_row("SELECT epoch FROM app_install_capabilities WHERE install_id=? AND digest=? AND state='approved'",params![install_id,bundle_digest],|r|r.get(0)).optional()?.ok_or_else(||Error::rejected("installation capability approval is absent or stale"))?;
        let owner = self.agent_in(&tx, owner_pm)?;
        if owner.role != "pm" {
            return Err(Error::rejected("run owner must be an existing PM"));
        }
        let mut assignments = BTreeMap::new();
        for step in &workflow.steps {
            let agent = self.agent_in(&tx, &step.assignee)?;
            let generation = agent.generation.clone().ok_or_else(|| {
                Error::rejected("local worker must have a registered endpoint generation")
            })?;
            let group = agent
                .params
                .as_ref()
                .and_then(|p| p.get("upstream"))
                .and_then(Value::as_str);
            if agent.alias != owner_pm && group != Some(owner_pm) {
                return Err(Error::rejected(
                    "local worker must belong to the run owner group",
                ));
            }
            assignments.insert(step.id.clone(),json!({"alias":agent.alias,"generation":generation,"role":agent.role,"provider":agent.provider,"endpoint_kind":agent.endpoint_kind}));
        }
        let snapshot = json!({"schema":1,"install_id":install_id,"bundle_digest":bundle_digest,"epoch":epoch,"workflow":workflow,"inputs":inputs,"assignments":assignments,"owner_pm":owner_pm,"project_link":project_link,"artifact_policy":{"types":["text/plain","text/markdown"],"max_bytes":ARTIFACT_BYTES,"aggregate_bytes":RUN_ARTIFACT_BYTES}});
        let digest = material_digest(&snapshot);
        if let Some((id, existing)) = tx
            .query_row(
                "SELECT id,snapshot_digest FROM app_runs WHERE install_id=? AND request_id=?",
                params![install_id, request_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
        {
            if existing != digest {
                return Err(Error::rejected(
                    "request ID already has a different immutable snapshot",
                ));
            }
            tx.commit()?;
            drop(conn);
            return self.app_run_show(&id);
        }
        let id = format!("run-{}", uuid::Uuid::new_v4().simple());
        tx.execute("INSERT INTO jobs(id,title,spec_path,spec_sha256,pm_alias,state,max_revisions,created,updated) VALUES(?,?,?,?,?,'open',1,?,?)",params![id,workflow.title,"app-run",digest,owner_pm,now(),now()])?;
        tx.execute(
            "INSERT INTO app_runs VALUES(?,?,?,?,?,?,?,?,?,'awaiting_approval',NULL,?,?)",
            params![
                id,
                install_id,
                epoch,
                bundle_digest,
                snapshot.to_string(),
                digest,
                project_link,
                owner_pm,
                request_id,
                now(),
                now()
            ],
        )?;
        for step in &workflow.steps {
            let task = format!("{id}-{}", step.id);
            let generation = assignments[&step.id]["generation"].as_str().unwrap();
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
        tx.commit()?;
        drop(conn);
        self.app_run_show(&id)
    }
    pub fn app_run_show(&self, id: &str) -> Result<Value> {
        let conn = self.conn();
        Self::app_run_show_in(&conn, id)
    }
    fn app_run_show_in(conn: &Connection, id: &str) -> Result<Value> {
        let mut value=conn.query_row("SELECT install_id,epoch,snapshot,snapshot_digest,project_link,state,approved_digest FROM app_runs WHERE id=?",[id],|r|Ok(json!({"id":id,"install_id":r.get::<_,String>(0)?,"epoch":r.get::<_,i64>(1)?,"snapshot":r.get::<_,String>(2)?,"snapshot_digest":r.get::<_,String>(3)?,"project_link":r.get::<_,Option<String>>(4)?,"state":r.get::<_,String>(5)?,"approved_digest":r.get::<_,Option<String>>(6)?}))).optional()?.ok_or_else(||Error::rejected("unknown app run"))?;
        value["snapshot"] = serde_json::from_str(value["snapshot"].as_str().unwrap())
            .map_err(|e| Error::internal(e.to_string()))?;
        value["steps"]=Value::Array(conn.prepare("SELECT step_id,task_id,state,message_id FROM app_run_steps WHERE run_id=? ORDER BY step_id")?.query_map([id],|r|Ok(json!({"step_id":r.get::<_,String>(0)?,"task_id":r.get::<_,String>(1)?,"state":r.get::<_,String>(2)?,"message_id":r.get::<_,Option<String>>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?);
        value["artifacts"]=Value::Array(conn.prepare("SELECT id,step_id,digest,media_type,length(content) FROM app_run_artifacts WHERE run_id=? ORDER BY step_id")?.query_map([id],|r|Ok(json!({"id":r.get::<_,String>(0)?,"step_id":r.get::<_,String>(1)?,"digest":r.get::<_,String>(2)?,"media_type":r.get::<_,String>(3)?,"size":r.get::<_,i64>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?);
        value["reviews"]=Value::Array(conn.prepare("SELECT step_id,artifact_digest,reviewer,decision,rationale FROM app_run_reviews WHERE run_id=?")?.query_map([id],|r|Ok(json!({"step_id":r.get::<_,String>(0)?,"artifact_digest":r.get::<_,String>(1)?,"reviewer":r.get::<_,String>(2)?,"decision":r.get::<_,String>(3)?,"rationale":r.get::<_,String>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?);
        Ok(value)
    }
    pub fn app_run_list(&self, install_id: Option<&str>) -> Result<Value> {
        let ids = self
            .conn()
            .prepare(
                "SELECT id FROM app_runs WHERE (?1 IS NULL OR install_id=?1) ORDER BY created",
            )?
            .query_map([install_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!({"runs":ids.iter().map(|id|self.app_run_show(id)).collect::<Result<Vec<_>>>()?}))
    }
    pub fn app_run_decide(&self, id: &str, digest: Option<&str>, cancel: bool) -> Result<Value> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
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
        } else {
            if state != "awaiting_approval" || digest != run["snapshot_digest"].as_str() {
                return Err(Error::rejected(
                    "execution decision needs the pending immutable snapshot digest",
                ));
            }
            Self::app_current_in(
                &tx,
                &run,
                run["snapshot"]["bundle_digest"].as_str().unwrap(),
            )?;
            tx.execute("UPDATE app_runs SET state='approved',approved_digest=snapshot_digest,updated=? WHERE id=?",params![now(),id])?;
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
        tx.commit()?;
        drop(conn);
        self.app_run_show(id)
    }
    fn app_current_in(conn: &Connection, run: &Value, bundle: &str) -> Result<()> {
        let found=conn.query_row("SELECT 1 FROM app_install_capabilities WHERE install_id=? AND epoch=? AND digest=? AND state='approved'",params![run["install_id"].as_str(),run["epoch"].as_i64(),bundle], |_|Ok(())).optional()?;
        if found.is_none() || run["snapshot"]["bundle_digest"].as_str() != Some(bundle) {
            return Err(Error::rejected(
                "app capability epoch or bundle digest is stale",
            ));
        }
        Ok(())
    }
}

impl Store {
    /// Called only while the daemon holds the installation's PM lock. SQL
    /// rechecks the epoch and dependency state in the enqueue transaction.
    pub fn app_run_dispatch(&self, id: &str, current_bundle: &str) -> Result<Value> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let run = Self::app_run_show_in(&tx, id)?;
        Self::app_current_in(&tx, &run, current_bundle)?;
        if !matches!(run["state"].as_str(), Some("approved" | "running"))
            || run["approved_digest"] != run["snapshot_digest"]
        {
            return Err(Error::rejected("run execution approval is absent or stale"));
        }
        let rows=tx.prepare("SELECT step_id,task_id,spec,generation FROM app_run_steps WHERE run_id=? AND state='pending' ORDER BY step_id")?.query_map([id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
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
                if let Some(artifact)=tx.query_row("SELECT id,digest,media_type,length(content) FROM app_run_artifacts WHERE run_id=? AND step_id=?",params![id,dep],|r|Ok(json!({"artifact_id":r.get::<_,String>(0)?,"producer_step_id":dep,"revision":1,"sha256":r.get::<_,String>(1)?,"media_type":r.get::<_,String>(2)?,"size":r.get::<_,i64>(3)?}))).optional()? {dependencies.push(artifact);}
            }
            if !ready {
                continue;
            }
            let worker = self.agent_in(&tx, &step.assignee)?;
            let expected = &run["snapshot"]["assignments"][&step_id];
            if worker.generation.as_deref() != Some(&generation)
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
            let envelope = json!({"schema":1,"run_id":id,"step_id":step_id,"revision":1,"kind":step.kind,"instruction":step.instruction,"dependencies":dependencies,"result_contract":"Return a JSON envelope in text with schema=1, kind, run_id, step_id, revision. Producer: outcome=succeeded, artifacts=[{media_type:text/markdown,text:...}]. Reviewer: producer_step_id, producer_revision=1, artifact_sha256, decision=approve|revise, rationale. Fetch dependencies with cadence app run artifact using the active message/turn token. No outward effects authorized.","max_artifact_bytes":ARTIFACT_BYTES});
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
        tx.commit()?;
        drop(conn);
        self.app_run_show(id)
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
        let conn = self.conn();
        let (run_id, producer_step, digest, media_type, bytes): (
            String,
            String,
            String,
            String,
            Vec<u8>,
        ) = conn
            .query_row(
                "SELECT run_id,step_id,digest,media_type,content FROM app_run_artifacts WHERE id=?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?
            .ok_or_else(|| Error::rejected("artifact unavailable"))?;
        let run = Self::app_run_show_in(&conn, &run_id)?;
        Self::app_current_in(&conn, &run, current_bundle)?;
        if let Some((message, token)) = turn {
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
                || generation.as_deref() != assigned["generation"].as_str()
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
        if bytes.len() > ARTIFACT_BYTES {
            return Err(Error::rejected("artifact exceeds its limit"));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| Error::internal("stored text artifact is not UTF-8"))?;
        Ok(json!({"id":id,"digest":digest,"media_type":media_type,"size":text.len(),"text":text}))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextArtifact {
    media_type: String,
    text: String,
}
#[derive(Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum LocalResult {
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
        decision: String,
        rationale: String,
    },
}
impl Store {
    /// Material transitions occur only on the actual active app kickoff. This
    /// transaction records durable eligibility; it never acquires the PM lock.
    pub(super) fn app_run_finished_in(
        &self,
        tx: &Connection,
        message: &Message,
        status: &str,
        result: &Value,
        authenticated: bool,
    ) -> Result<bool> {
        let Some(task_id) = message.task_id.as_deref() else {
            return Ok(false);
        };
        let Some((run_id,step_id,spec,generation,state,message_id))=tx.query_row("SELECT run_id,step_id,spec,generation,state,message_id FROM app_run_steps WHERE task_id=?",[task_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,Option<String>>(5)?))).optional()? else{return Ok(false)};
        if !authenticated {
            return Ok(true);
        }; // reconciliation is transport history only
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
        let token = current
            .turn_id
            .as_deref()
            .ok_or_else(|| Error::rejected("app material result needs its active turn"))?;
        if worker.generation.as_deref() != Some(&generation)
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
            serde_json::from_str::<LocalResult>(text)
        } else {
            serde_json::from_value::<LocalResult>(result.clone())
        };
        let Ok(decoded) = decoded else {
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
                    && rationale.len() <= 16 * 1024
                    && matches!(decision.as_str(), "approve" | "revise")
                {
                    if let Some((id,digest,producer))=tx.query_row("SELECT id,digest,producer FROM app_run_artifacts WHERE run_id=? AND step_id=?",params![run_id,producer_step_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional()? {
                        if digest==artifact_sha256 && producer!=message.alias {
                            tx.execute("INSERT INTO app_run_reviews VALUES(?,?,?,?,?,?,?,?)",params![run_id,step_id,id,digest,message.alias,message.id,decision,rationale])?;
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
        let result_digest = material_digest(result);
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
        tx: &Connection,
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
    pub fn app_artifact_installation(&self, id: &str) -> Result<String> {
        Ok(self.conn().query_row("SELECT r.install_id FROM app_run_artifacts a JOIN app_runs r ON r.id=a.run_id WHERE a.id=?",[id],|r|r.get(0)).optional()?.ok_or_else(||Error::rejected("artifact unavailable"))?)
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

fn local_token_current(provider: &str, kind: &str, generation: Option<&str>, token: &str) -> bool {
    let Some(spec) = crate::adapter::registry::spec_opt(provider, kind) else {
        return false;
    };
    generation.is_some()
        && !token.is_empty()
        && (spec.turn_token.is_none()
            || crate::adapter::registry::turn_token_current(provider, kind, generation, token))
}
