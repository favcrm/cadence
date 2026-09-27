//! Operator-owned local app lifecycle and turn-bound dependency artifacts.
use super::*;
use crate::issue::app_catalog::workspace;
use crate::store::app_runs::{LocalRunRequest, LocalWorkflow};
use std::collections::BTreeMap;

impl Shared {
    pub(super) fn rpc_app_local(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let allowed: &[&str] = match method {
            "app_local_install_approve" | "app_local_install_revoke" => &["install_id", "digest"],
            "app_run_create" => &[
                "install_id",
                "workflow",
                "inputs",
                "request_id",
                "owner_pm",
                "project_link",
            ],
            "app_run_approve" => &["run_id", "digest"],
            "app_run_cancel" | "app_run_dispatch" | "app_run_show" => &["run_id"],
            "app_run_list" => &["install_id"],
            "app_run_artifact" => &["artifact_id", "message", "token"],
            _ => return Err(Error::rejected("unknown app lifecycle method")),
        };
        let fields = params
            .as_object()
            .ok_or_else(|| Error::rejected("app lifecycle payload must be an object"))?;
        if fields.keys().any(|k| !allowed.contains(&k.as_str())) {
            return Err(Error::rejected(
                "app lifecycle payload has unsupported fields",
            ));
        }
        for field in ["project_link", "install_id"] {
            if fields.get(field).is_some_and(|value| !value.is_string()) {
                return Err(Error::rejected(
                    "optional app references must be strings when present",
                ));
            }
        }
        if method == "app_run_artifact" && fields.contains_key("message") {
            let caller = self.agent_caller(peer_pid, "app dependency artifact")?;
            let AgentCaller::Agent(alias) = caller else {
                return Err(Error::rejected(
                    "dependency artifact requires its assigned worker",
                ));
            };
            let message = required_str(params, "message")?;
            let token = required_str(params, "token")?;
            if self.store.message(message)?.map(|m| m.alias) != Some(alias) {
                return Err(Error::rejected("artifact turn belongs to another worker"));
            }
            return self.app_artifact_current(
                required_str(params, "artifact_id")?,
                Some((message, token)),
            );
        }
        self.operator_connection("app local lifecycle", params, peer_pid)?;
        match method {
            "app_local_install_approve" | "app_local_install_revoke" => {
                let pm = self.pm_at(&self.pm_dir()?)?;
                workspace::with_runtime_snapshot(
                    &pm,
                    required_str(params, "install_id")?,
                    |row, files| {
                        let digest = required_str(params, "digest")?;
                        if row["digest"].as_str() != Some(digest) {
                            return Err(Error::rejected("installation digest is stale"));
                        }
                        if method == "app_local_install_approve" {
                            let mut count = 0;
                            for (name, text) in files {
                                if name.starts_with("workflows/") {
                                    LocalWorkflow::validate_template(text)?;
                                    count += 1;
                                }
                            }
                            if count == 0 {
                                return Err(Error::rejected(
                                    "installed app has no supported local workflow",
                                ));
                            }
                        }
                        self.store.app_capability_decide(
                            required_str(params, "install_id")?,
                            digest,
                            method == "app_local_install_approve",
                        )
                    },
                )
            }
            "app_run_create" => {
                let id = required_str(params, "install_id")?;
                let name = required_str(params, "workflow")?;
                if !crate::issue::model::valid_tag(name) {
                    return Err(Error::rejected("invalid installed workflow name"));
                }
                let inputs: BTreeMap<String, String> =
                    serde_json::from_value(params.get("inputs").cloned().unwrap_or(json!({})))
                        .map_err(|_| Error::rejected("inputs must be a string map"))?;
                if serde_json::to_vec(&inputs)
                    .map_err(|e| Error::internal(e.to_string()))?
                    .len()
                    > 32 * 1024
                {
                    return Err(Error::rejected("inputs exceed encoded byte limit"));
                }
                let pm = self.pm_at(&self.pm_dir()?)?;
                workspace::with_runtime_snapshot(&pm, id, |row, files| {
                    let text = files.get(&format!("workflows/{name}.md")).ok_or_else(|| {
                        Error::rejected("workflow is not in this installed bundle")
                    })?;
                    let workflow = LocalWorkflow::parse(text, &inputs)?;
                    self.store.app_run_create(LocalRunRequest {
                        install_id: id,
                        bundle_digest: row["digest"].as_str().unwrap(),
                        workflow: &workflow,
                        inputs: &inputs,
                        request_id: required_str(params, "request_id")?,
                        owner_pm: required_str(params, "owner_pm")?,
                        project_link: optional_str(params, "project_link"),
                    })
                })
            }
            "app_run_approve" => {
                let id = required_str(params, "run_id")?;
                self.with_app_run_current(id, |_| {
                    self.store
                        .app_run_decide(id, Some(required_str(params, "digest")?), false)
                })
            }
            "app_run_cancel" => {
                self.store
                    .app_run_decide(required_str(params, "run_id")?, None, true)
            }
            "app_run_dispatch" => self.dispatch_app_run(required_str(params, "run_id")?),
            "app_run_show" => self.store.app_run_show(required_str(params, "run_id")?),
            "app_run_list" => self.store.app_run_list(optional_str(params, "install_id")),
            "app_run_artifact" => {
                if fields.contains_key("token") {
                    return Err(Error::rejected("token requires its assigned message"));
                }
                self.app_artifact_current(required_str(params, "artifact_id")?, None)
            }
            _ => Err(Error::rejected("unknown app lifecycle method")),
        }
    }
    pub(super) fn with_app_run_current<T>(
        &self,
        id: &str,
        callback: impl FnOnce(&str) -> Result<T>,
    ) -> Result<T> {
        let run = self.store.app_run_show(id)?;
        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_runtime_snapshot(&pm, run["install_id"].as_str().unwrap(), |row, _| {
            callback(row["digest"].as_str().unwrap())
        })
    }
    pub(super) fn dispatch_app_run(&self, id: &str) -> Result<Value> {
        let result =
            self.with_app_run_current(id, |digest| self.store.app_run_dispatch(id, digest))?;
        for step in result["snapshot"]["workflow"]["steps"].as_array().unwrap() {
            self.notify_agent(step["assignee"].as_str().unwrap());
        }
        self.wake();
        Ok(result)
    }
    pub(super) fn advance_app_runs(&self) {
        if self.draining() {
            return;
        }
        if let Ok(runs) = self.store.app_run_pending() {
            for (id, _, _) in runs {
                if let Err(Error::Rejected(_)) = self.dispatch_app_run(&id) {
                    let _ = self.store.app_run_invalidate(&id);
                }
            }
        }
    }
    fn app_artifact_current(&self, id: &str, turn: Option<(&str, &str)>) -> Result<Value> {
        let install = self.store.app_artifact_installation(id)?;
        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_runtime_snapshot(&pm, &install, |row, _| {
            self.store
                .app_artifact_with_digest(id, turn, row["digest"].as_str().unwrap())
        })
    }
}

impl Shared {
    pub(super) fn take_app_aware(&self, alias: &str) -> Result<Take> {
        let head = self.store.queued_head(alias)?;
        if let Some(message) = head.filter(|m| m.source == "app_run_dispatch") {
            let admission = (|| {
                let (_, install) = self
                    .store
                    .app_message_installation(&message.id)?
                    .ok_or_else(|| Error::rejected("app message association is absent"))?;
                let pm = self.pm_at(&self.pm_dir()?)?;
                workspace::with_runtime_snapshot(&pm, &install, |row, _| {
                    self.store.take_queued_app_proven(
                        alias,
                        Some((&message.id, row["digest"].as_str().unwrap())),
                    )
                })
            })();
            match admission {
                Ok(take) => Ok(take),
                Err(_) => {
                    self.store.reject_app_submission(&message.id)?;
                    Ok(Take::Empty)
                }
            }
        } else {
            self.store.take_queued(alias)
        }
    }
    pub(super) fn admit_app_submission(&self, message: &Message) -> Result<()> {
        if message.source != "app_run_dispatch" {
            return Ok(());
        }
        let (_, install) = self
            .store
            .app_message_installation(&message.id)?
            .ok_or_else(|| Error::rejected("app association is absent"))?;
        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_runtime_snapshot(&pm, &install, |row, _| {
            self.store
                .app_message_admit(message, row["digest"].as_str().unwrap())
        })
    }
}
