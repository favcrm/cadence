//! Daemon RPC handlers for Routines, Connection Policies, Inline Credential
//! Resume, and Staged Routine/Wiki Proposals (CAD-2317).
//!
//! Security & Gate invariants:
//! - Every mutating routine or connection-policy verb (`routine_create`,
//!   `routine_update`, `routine_approve`, `routine_run`, `routine_pause`,
//!   `routine_resume`, `routine_remove`, `routine_proposal_decide`,
//!   `connection_policy_set`, `credential_request_continue`) proves the
//!   operator connection via `self.operator_connection(...)` before reading or
//!   mutating any durable state.
//! - `routine_run` verifies live `markdown_path` (`fnv1a:<16hex>`) ==
//!   `approved_rev`, live `script_path` (`sha256`) == `approved_sha256`, and
//!   shared-connection `allow_external_triggers` BEFORE inserting a
//!   `routine_runs` row, spawning a script, or queuing any message — a tampered
//!   or unapproved target is refused with zero durable or external side effects.
//! - Switching a `shared` connection to `personal` revokes its shared
//!   credential first (`revoke_inner_expected`), leaving zero shared secret
//!   material attached to the personal policy.

use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::*;
use crate::error::{Error, Result};
use crate::issue::Pm;
use crate::store::routines::{
    ChecklistStep, ConnectionPolicyInput, Routine, RoutineCreate, RoutineUpdate, RunOutcome,
    SlotAdmission,
};

const MAX_SCRIPT_BYTES: usize = 1024 * 1024;
const MAX_SCRIPT_OUTPUT_BYTES: usize = 64 * 1024;
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(30);

fn normalize_sha256_hex(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    let hex = trimmed.strip_prefix("sha256:").unwrap_or(trimmed);
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::rejected(
            "script digest must be a 64-character hexadecimal SHA-256 value",
        ));
    }
    Ok(hex.to_ascii_lowercase())
}

fn normalize_rev(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    let Some(hex) = trimmed.strip_prefix("fnv1a:") else {
        return Err(Error::rejected(
            "markdown revision must have format 'fnv1a:<16-hex>'",
        ));
    };
    if hex.len() != 16 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::rejected(
            "markdown revision must have format 'fnv1a:<16-hex>'",
        ));
    }
    Ok(format!("fnv1a:{}", hex.to_ascii_lowercase()))
}

fn ensure_no_symlinks_under(base: &Path, rel: &Path) -> Result<PathBuf> {
    let mut cur = base.to_path_buf();
    for comp in rel.components() {
        match comp {
            Component::Normal(seg) => {
                let s = seg
                    .to_str()
                    .ok_or_else(|| Error::rejected("path contains non-UTF-8 segment"))?;
                if s.is_empty() || s.starts_with('.') {
                    return Err(Error::rejected(
                        "path segments may not be empty or dot-prefixed",
                    ));
                }
                cur.push(seg);
                let meta = std::fs::symlink_metadata(&cur).map_err(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        Error::rejected(format!("target file '{}' does not exist", rel.display()))
                    } else {
                        Error::from(e)
                    }
                })?;
                if meta.file_type().is_symlink() {
                    return Err(Error::rejected(
                        "symlinks are not permitted in routine target paths",
                    ));
                }
            }
            _ => {
                return Err(Error::rejected(
                    "routine target path must be a normalized relative path without '..'",
                ))
            }
        }
    }
    Ok(cur)
}

fn parse_checklist(params: &Value) -> Result<Vec<ChecklistStep>> {
    let Some(arr) = params.get("checklist") else {
        return Ok(Vec::new());
    };
    if arr.is_null() {
        return Ok(Vec::new());
    }
    let steps: Vec<ChecklistStep> = serde_json::from_value(arr.clone())
        .map_err(|e| Error::rejected(format!("invalid 'checklist' array: {e}")))?;
    if steps.len() > 32 {
        return Err(Error::rejected("checklist may contain at most 32 steps"));
    }
    Ok(steps)
}

fn parse_allowed_agents(params: &Value) -> Result<Vec<String>> {
    let Some(arr) = params.get("allowed_agents") else {
        return Ok(Vec::new());
    };
    if arr.is_null() {
        return Ok(Vec::new());
    }
    let list = arr
        .as_array()
        .ok_or_else(|| Error::rejected("'allowed_agents' must be an array of strings"))?;
    let mut out = Vec::with_capacity(list.len());
    for item in list {
        let alias = item
            .as_str()
            .ok_or_else(|| Error::rejected("'allowed_agents' items must be strings"))?;
        crate::proto::identifier(alias, "Allowed agent alias")?;
        if !out.iter().any(|existing| existing == alias) {
            out.push(alias.to_string());
        }
    }
    Ok(out)
}

/// Evaluate whether script stdout indicates zero actionable items (`skipped_empty`)
/// vs actionable items (`(true, structured_json_value)`).
fn classify_script_stdout(stdout: &[u8]) -> (bool, Option<Value>) {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return (false, None);
    }
    if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
        if let Some(obj) = val.as_object() {
            if obj.get("triggered").and_then(Value::as_bool) == Some(false) {
                return (false, Some(val));
            }
            if let Some(items) = obj.get("items").and_then(Value::as_array) {
                if items.is_empty() {
                    return (false, Some(val));
                }
                return (true, Some(val));
            }
            if obj.is_empty() {
                return (false, Some(val));
            }
        }
        if let Some(arr) = val.as_array() {
            if arr.is_empty() {
                return (false, Some(val));
            }
            return (true, Some(val));
        }
        return (true, Some(val));
    }
    (true, Some(json!({ "text": trimmed })))
}

impl Shared {
    pub(super) fn resolve_routine_markdown(&self, markdown_path: &str) -> Result<(String, String)> {
        let pm = self.pm()?;
        Self::resolve_markdown_in(&pm, markdown_path)
    }

    fn resolve_markdown_in(pm: &Pm, markdown_path: &str) -> Result<(String, String)> {
        let raw = markdown_path.trim();
        if let Some(wiki_sub) = raw.strip_prefix("wiki:") {
            let norm = crate::wiki::normalize(wiki_sub)?;
            if norm.is_empty() || !norm.ends_with(".md") {
                return Err(Error::rejected(
                    "wiki markdown_path must reference a '.md' page (for example 'wiki:global/triage.md')",
                ));
            }
            if let Ok(page) = crate::wiki::read(pm, &crate::wiki::Caller::Operator, &norm) {
                if let Some(text) = page.get("text").and_then(Value::as_str) {
                    let rev = crate::wiki::rev_bytes(text.as_bytes());
                    return Ok((text.to_string(), rev));
                }
            }
            let wiki_root = pm.dir.join("wiki");
            let full = ensure_no_symlinks_under(&wiki_root, Path::new(&norm))?;
            let bytes = std::fs::read(&full).map_err(|_| {
                Error::rejected(format!("wiki markdown target '{raw}' does not exist"))
            })?;
            let text = String::from_utf8(bytes)
                .map_err(|_| Error::rejected("wiki markdown target must be valid UTF-8"))?;
            let rev = crate::wiki::rev_bytes(text.as_bytes());
            return Ok((text, rev));
        }

        if !raw.ends_with(".md") {
            return Err(Error::rejected(
                "markdown_path must reference 'wiki:<path>.md' or '[<project>/]workflows/<name>.md'",
            ));
        }
        let segs: Vec<&str> = raw.split('/').collect();
        let valid_workflow = matches!(
            segs.as_slice(),
            ["workflows", file] | [_, "workflows", file] if !file.is_empty()
        );
        if !valid_workflow {
            return Err(Error::rejected(
                "markdown_path must reference 'wiki:<path>.md' or '[<project>/]workflows/<name>.md'",
            ));
        }
        let full = ensure_no_symlinks_under(&pm.dir, Path::new(raw))?;
        let bytes = std::fs::read(&full).map_err(|_| {
            Error::rejected(format!("workflow markdown target '{raw}' does not exist"))
        })?;
        let text = String::from_utf8(bytes)
            .map_err(|_| Error::rejected("workflow markdown target must be valid UTF-8"))?;
        let rev = crate::wiki::rev_bytes(text.as_bytes());
        Ok((text, rev))
    }

    pub(super) fn resolve_routine_script(&self, script_path: &str) -> Result<(Vec<u8>, String)> {
        let pm = self.pm()?;
        let raw = script_path.trim();
        if raw.is_empty() {
            return Err(Error::rejected("script_path must not be empty"));
        }
        let path = Path::new(raw);
        let scripts_dir = self.state_dir.join("scripts");
        let full = if path.is_absolute() {
            let pm_canon = pm.dir.canonicalize()?;
            let scripts_canon = scripts_dir.canonicalize().ok();
            if let Ok(rel) = path
                .strip_prefix(&pm.dir)
                .or_else(|_| path.strip_prefix(&pm_canon))
            {
                ensure_no_symlinks_under(&pm.dir, rel)?
            } else if let Ok(rel) = path.strip_prefix(&scripts_dir).or_else(|err| {
                scripts_canon
                    .as_deref()
                    .ok_or(err)
                    .and_then(|sc| path.strip_prefix(sc))
            }) {
                ensure_no_symlinks_under(&scripts_dir, rel)?
            } else {
                return Err(Error::rejected(
                    "script_path must reside inside the workspace tracker or state_dir/scripts",
                ));
            }
        } else {
            match ensure_no_symlinks_under(&pm.dir, path) {
                Ok(found) => found,
                Err(pm_err) => {
                    if let Ok(rel) = path.strip_prefix("scripts") {
                        if scripts_dir.is_dir() {
                            if let Ok(found) = ensure_no_symlinks_under(&scripts_dir, rel) {
                                found
                            } else {
                                return Err(pm_err);
                            }
                        } else {
                            return Err(pm_err);
                        }
                    } else {
                        return Err(pm_err);
                    }
                }
            }
        };
        let meta = std::fs::symlink_metadata(&full)
            .map_err(|_| Error::rejected(format!("script target '{raw}' does not exist")))?;
        if !meta.is_file() {
            return Err(Error::rejected("script_path must reference a regular file"));
        }
        if meta.len() > MAX_SCRIPT_BYTES as u64 {
            return Err(Error::rejected("script_path exceeds 1 MiB size limit"));
        }
        let bytes = std::fs::read(&full)?;
        let sha = format!("{:x}", Sha256::digest(&bytes));
        Ok((bytes, sha))
    }

    fn execute_verified_script(
        &self,
        routine_id: &str,
        due_instant: i64,
        verified_bytes: &[u8],
    ) -> std::result::Result<(Vec<u8>, bool), String> {
        let exec_root = self.state_dir.join("routine-exec");
        if let Err(e) = std::fs::create_dir_all(&exec_root) {
            return Err(format!("failed to create routine-exec dir: {e}"));
        }
        let _ = std::fs::set_permissions(&exec_root, std::fs::Permissions::from_mode(0o700));
        let run_dir = exec_root.join(format!("run-{}", uuid::Uuid::new_v4().simple()));
        if let Err(e) = std::fs::create_dir(&run_dir) {
            return Err(format!("failed to create per-run routine-exec dir: {e}"));
        }
        let _ = std::fs::set_permissions(&run_dir, std::fs::Permissions::from_mode(0o700));
        let tmp_script = run_dir.join("script.bin");
        {
            use std::io::Write;
            let mut file = match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o700)
                .open(&tmp_script)
            {
                Ok(f) => f,
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&run_dir);
                    return Err(format!("failed to stage verified script: {e}"));
                }
            };
            if let Err(e) = file
                .write_all(verified_bytes)
                .and_then(|()| file.sync_all())
            {
                let _ = std::fs::remove_dir_all(&run_dir);
                return Err(format!("failed to write verified script: {e}"));
            }
        }

        let workdir = self
            .pm()
            .map(|p| p.dir)
            .unwrap_or_else(|_| self.state_dir.clone());
        let mut cmd = if verified_bytes.starts_with(b"#!") {
            Command::new(&tmp_script)
        } else {
            let mut c = Command::new("/bin/sh");
            c.arg(&tmp_script);
            c
        };
        let safe_path = std::env::var_os("PATH")
            .unwrap_or_else(|| std::ffi::OsString::from("/usr/local/bin:/usr/bin:/bin"));
        // Strict environment allowlist: strips DENIED_ENV, CLOUD_SECRET_ENV,
        // SSH_AUTH_SOCK, and ambient user HOME credentials, and runs inside a
        // dedicated process group via `proc::run_bounded_limited` so timeouts
        // SIGKILL `-pgid` and never hang daemon pipe readers.
        cmd.current_dir(&workdir)
            .env_clear()
            .env("PATH", safe_path)
            .env("LANG", "C.UTF-8")
            .env("TZ", "UTC")
            .env("HOME", &run_dir)
            .env("TMPDIR", &run_dir)
            .env("CADENCE_ROUTINE_ID", routine_id)
            .env("CADENCE_ROUTINE_DUE", due_instant.to_string());

        let outcome =
            crate::proc::run_bounded_limited(&mut cmd, SCRIPT_TIMEOUT, MAX_SCRIPT_OUTPUT_BYTES);
        let _ = std::fs::remove_dir_all(&run_dir);

        match outcome {
            Ok((output, _bounds)) => {
                if output.status.code() == Some(10) {
                    return Ok((output.stdout, true));
                }
                if !output.status.success() {
                    let err_slice = &output.stderr[..output.stderr.len().min(4096)];
                    let err_text = String::from_utf8_lossy(err_slice).trim().to_string();
                    let detail = if err_text.is_empty() {
                        format!("script exited with status {}", output.status)
                    } else {
                        format!("script exited with status {}: {err_text}", output.status)
                    };
                    return Err(detail);
                }
                Ok((output.stdout, false))
            }
            Err(crate::proc::BoundedError::TimedOut { .. }) => {
                Err("routine script timed out after 30s".to_string())
            }
            Err(crate::proc::BoundedError::Spawn(e)) => {
                Err(format!("failed to spawn routine script: {e}"))
            }
            Err(crate::proc::BoundedError::Wait(e)) => {
                Err(format!("failed waiting for routine script: {e}"))
            }
        }
    }

    fn admit_routine_read(&self, op: &str, params: &Value, peer_pid: u32) -> Result<()> {
        if self.operator_connection(op, params, peer_pid).is_ok() {
            return Ok(());
        }
        if let Ok(super::identity::Caller::Agent(verified)) = self.caller_identity(peer_pid) {
            if crate::master::is_master(&verified.agent.alias) {
                return Ok(());
            }
        }
        self.operator_connection(op, params, peer_pid).map(|_| ())
    }

    pub(super) fn project_routine(&self, routine: &Routine) -> Result<Value> {
        let current_rev = routine
            .markdown_path
            .as_deref()
            .and_then(|p| self.resolve_routine_markdown(p).ok().map(|(_, rev)| rev));
        let current_sha = routine
            .script_path
            .as_deref()
            .and_then(|p| self.resolve_routine_script(p).ok().map(|(_, sha)| sha));
        let thread_live = self
            .store
            .destination_thread_live(routine.destination_thread.as_deref())?;
        Ok(routine.to_json(current_rev.as_deref(), current_sha.as_deref(), thread_live))
    }

    pub(super) fn rpc_routine_list(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.admit_routine_read("routine list", params, peer_pid)?;
        let agent = optional_str(params, "agent_alias").or_else(|| optional_str(params, "agent"));
        let rows = self.store.routine_list(agent)?;
        let mut routines = Vec::with_capacity(rows.len());
        for row in &rows {
            routines.push(self.project_routine(row)?);
        }
        let proposals = self
            .store
            .staged_proposals_pending()?
            .into_iter()
            .map(|p| p.to_json())
            .collect::<Vec<_>>();
        let waiting_runs = self
            .store
            .routine_runs_waiting_approval()?
            .into_iter()
            .filter(|(r, _)| agent.is_none_or(|a| r.agent_alias == a))
            .map(|(routine, run)| {
                let mut item = run.to_json(false);
                item["routine_name"] = json!(routine.name);
                item["agent_alias"] = json!(routine.agent_alias);
                item
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "routines": routines,
            "staged_proposals": proposals,
            "waiting_runs": waiting_runs,
        }))
    }

    pub(super) fn rpc_routine_show(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.admit_routine_read("routine show", params, peer_pid)?;
        let id = required_str(params, "id")?;
        let routine = self.store.routine_get(id)?;
        let mut projected = self.project_routine(&routine)?;
        let runs = self
            .store
            .routine_runs(id, 25)?
            .into_iter()
            .map(|r| r.to_json(false))
            .collect::<Vec<_>>();
        projected["runs"] = json!(runs);
        Ok(json!({ "routine": projected }))
    }

    pub(super) fn rpc_routine_create(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("routine create", params, peer_pid)?;
        let name = required_str(params, "name")?.to_string();
        let agent_alias = params
            .get("agent_alias")
            .or_else(|| params.get("agent"))
            .and_then(Value::as_str)
            .ok_or_else(|| Error::rejected("routine create requires 'agent_alias'"))?
            .to_string();
        let trigger_kind = optional_str(params, "trigger_kind")
            .unwrap_or("schedule")
            .to_string();
        let cron = optional_str(params, "cron").map(str::to_string);
        let interval_mins = params.get("interval_mins").and_then(Value::as_i64);
        let mut timezone = optional_str(params, "timezone").map(str::to_string);
        if trigger_kind == "schedule" {
            // CAD-2378: a schedule that names no zone runs in the company zone.
            timezone = Some(self.store.schedule_zone_or_company(timezone)?);
        }
        let event_source = optional_str(params, "event_source").map(str::to_string);
        let target_kind = required_str(params, "target_kind")?.to_string();
        let markdown_path = optional_str(params, "markdown_path").map(str::to_string);
        let script_path = optional_str(params, "script_path").map(str::to_string);
        let destination_thread = optional_str(params, "destination_thread").map(str::to_string);
        let connection_account = optional_str(params, "connection_account").map(str::to_string);
        let checklist = parse_checklist(params)?;
        let inputs = params.get("inputs").cloned().unwrap_or_else(|| json!({}));

        // Eagerly validate target files exist and conform to grammar at creation time.
        if matches!(target_kind.as_str(), "markdown" | "script_then_markdown") {
            let md = markdown_path.as_deref().ok_or_else(|| {
                Error::rejected("routine create requires 'markdown_path' for markdown target")
            })?;
            let _ = self.resolve_routine_markdown(md)?;
        }
        if matches!(target_kind.as_str(), "script" | "script_then_markdown") {
            let sp = script_path.as_deref().ok_or_else(|| {
                Error::rejected("routine create requires 'script_path' for script target")
            })?;
            let _ = self.resolve_routine_script(sp)?;
        }

        let now_epoch = crate::issue::time::now_epoch();
        let created = self.store.routine_create(RoutineCreate {
            name,
            agent_alias,
            trigger_kind,
            cron,
            interval_mins,
            timezone,
            event_source,
            target_kind,
            markdown_path,
            script_path,
            destination_thread,
            connection_account,
            checklist,
            inputs,
            now_epoch,
        })?;
        self.wake();
        Ok(json!({
            "ok": true,
            "routine": self.project_routine(&created)?,
        }))
    }

    pub(super) fn rpc_routine_update(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("routine update", params, peer_pid)?;
        let id = required_str(params, "id")?;
        let opt_nullable_str = |key: &str| -> Option<Option<String>> {
            params.get(key).map(|v| v.as_str().map(str::to_string))
        };
        if let Some(Some(ref md)) = opt_nullable_str("markdown_path") {
            let _ = self.resolve_routine_markdown(md)?;
        }
        if let Some(Some(ref sp)) = opt_nullable_str("script_path") {
            let _ = self.resolve_routine_script(sp)?;
        }
        let patch = RoutineUpdate {
            name: optional_str(params, "name").map(str::to_string),
            trigger_kind: optional_str(params, "trigger_kind").map(str::to_string),
            cron: opt_nullable_str("cron"),
            interval_mins: params.get("interval_mins").map(Value::as_i64),
            timezone: optional_str(params, "timezone").map(str::to_string),
            event_source: opt_nullable_str("event_source"),
            target_kind: optional_str(params, "target_kind").map(str::to_string),
            markdown_path: opt_nullable_str("markdown_path"),
            script_path: opt_nullable_str("script_path"),
            destination_thread: opt_nullable_str("destination_thread"),
            connection_account: opt_nullable_str("connection_account"),
            checklist: params
                .get("checklist")
                .map(|_| parse_checklist(params))
                .transpose()?,
            inputs: params.get("inputs").cloned(),
            now_epoch: crate::issue::time::now_epoch(),
        };
        let updated = self.store.routine_update(id, patch)?;
        self.wake();
        Ok(json!({
            "ok": true,
            "routine": self.project_routine(&updated)?,
        }))
    }

    pub(super) fn rpc_routine_approve(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("routine approve", params, peer_pid)?;
        let id = required_str(params, "id")?;
        let routine = self.store.routine_get(id)?;

        let approved_rev = if routine.requires_markdown() {
            let expect = params
                .get("expect_rev")
                .or_else(|| params.get("rev"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    Error::rejected(
                        "routine approve requires 'expect_rev' matching the live markdown file",
                    )
                })?;
            let normalized = normalize_rev(expect)?;
            let md_path = routine.markdown_path.as_deref().unwrap_or("");
            let (_, live_rev) = self.resolve_routine_markdown(md_path)?;
            if normalized != live_rev {
                return Err(Error::rejected(format!(
                    "stale_revision: supplied expect_rev '{normalized}' does not match live revision '{live_rev}'"
                )));
            }
            Some(live_rev)
        } else {
            None
        };

        let approved_sha = if routine.requires_script() {
            let digest = params
                .get("digest")
                .or_else(|| params.get("sha256"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    Error::rejected(
                        "routine approve requires 'digest' (sha256) matching the live script file",
                    )
                })?;
            let normalized = normalize_sha256_hex(digest)?;
            let sp_path = routine.script_path.as_deref().unwrap_or("");
            let (_, live_sha) = self.resolve_routine_script(sp_path)?;
            if normalized != live_sha {
                return Err(Error::rejected(format!(
                    "digest_mismatch: supplied digest '{normalized}' does not match live script sha256 '{live_sha}'"
                )));
            }
            Some(live_sha)
        } else {
            None
        };

        let updated = self.store.routine_approve(
            id,
            approved_rev,
            approved_sha,
            crate::issue::time::now_epoch(),
        )?;
        self.wake();
        Ok(json!({
            "ok": true,
            "routine": self.project_routine(&updated)?,
        }))
    }

    pub(super) fn rpc_routine_run(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("routine run", params, peer_pid)?;
        let id = required_str(params, "id")?;
        let trigger_source = optional_str(params, "trigger_source").unwrap_or("manual");
        if !matches!(trigger_source, "manual" | "schedule" | "external_event") {
            return Err(Error::rejected(
                "trigger_source must be 'manual', 'schedule', or 'external_event'",
            ));
        }
        let now_epoch = crate::issue::time::now_epoch();
        let due_instant = match params.get("due_instant").and_then(Value::as_i64) {
            Some(explicit) => explicit,
            None => self.store.next_adhoc_slot(id, now_epoch)?,
        };
        self.execute_routine_gated(id, due_instant, now_epoch, trigger_source)
    }

    fn verify_routine_connection_policy(
        &self,
        routine: &Routine,
        trigger_source: &str,
    ) -> Result<Option<(String, String)>> {
        let is_external_event = trigger_source == "external_event"
            || (routine.trigger_kind == "event" && trigger_source != "manual");
        let enrolled = self.store.platform_credentials().unwrap_or_default();

        if let Some(ref conn_ref) = routine.connection_account {
            let matched = self.store.connection_credential(conn_ref)?.or_else(|| {
                enrolled
                    .iter()
                    .find(|r| r.connection_id == *conn_ref || r.account == *conn_ref)
                    .cloned()
            });
            let (conn_id, provider, account) = if let Some(ref rec) = matched {
                (
                    rec.connection_id.as_str(),
                    rec.platform.as_str(),
                    rec.account.as_str(),
                )
            } else if let Some(rest) = conn_ref.strip_prefix("acct:") {
                let (prov, acct) = rest.split_once(':').unwrap_or(("", ""));
                (conn_ref.as_str(), prov, acct)
            } else {
                (conn_ref.as_str(), "", "")
            };

            let policy = self
                .store
                .connection_policy_resolve(conn_id, provider, account)?;
            if !policy.allowed_agents.is_empty()
                && !policy
                    .allowed_agents
                    .iter()
                    .any(|a| a == &routine.agent_alias)
            {
                return Err(Error::rejected(format!(
                    "connection_agent_denied: agent '{}' is not permitted to use connection '{}'",
                    routine.agent_alias, conn_id
                )));
            }
            if is_external_event && policy.visibility == "shared" && !policy.allow_external_triggers
            {
                return Err(Error::rejected(
                    "external_trigger_refused: shared workspace connection does not allow incoming external event triggers",
                ));
            }
            let out_prov = if provider.is_empty() {
                policy.provider.clone()
            } else {
                provider.to_string()
            };
            let out_acct = if account.is_empty() {
                conn_id.to_string()
            } else {
                account.to_string()
            };
            return Ok(Some((out_prov, out_acct)));
        }

        // Unpinned (`Default`) connection resolution + untrusted inbound event gate.
        let mut candidate_providers: Vec<String> = Vec::new();
        if let Some(ref src) = routine.event_source {
            let prefix = src
                .split([':', '.', '/'])
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            if !prefix.is_empty() {
                if matches!(prefix.as_str(), "email" | "gmail" | "mail") {
                    candidate_providers.push("smtp".to_string());
                }
                candidate_providers.push(prefix);
            }
        }

        // Prefer an enrolled personal `is_default` account allowed for this agent.
        for rec in &enrolled {
            if !candidate_providers.is_empty()
                && !candidate_providers.iter().any(|p| p == &rec.platform)
            {
                continue;
            }
            if let Ok(pol) = self.store.connection_policy_resolve(
                &rec.connection_id,
                &rec.platform,
                &rec.account,
            ) {
                let agent_ok = pol.allowed_agents.is_empty()
                    || pol.allowed_agents.iter().any(|a| a == &routine.agent_alias);
                if pol.visibility == "personal" && pol.is_default && agent_ok {
                    return Ok(Some((rec.platform.clone(), rec.account.clone())));
                }
            }
        }

        if is_external_event {
            let all_policies = self.store.connection_policies_all().unwrap_or_default();
            for rec in &enrolled {
                let matches_provider = if !candidate_providers.is_empty() {
                    candidate_providers.iter().any(|p| p == &rec.platform)
                } else {
                    self.store
                        .platform_grant(&routine.agent_alias, &rec.platform, &rec.account)
                        .ok()
                        .flatten()
                        .is_some()
                };
                if !matches_provider {
                    continue;
                }
                let pol = self.store.connection_policy_resolve(
                    &rec.connection_id,
                    &rec.platform,
                    &rec.account,
                )?;
                if pol.visibility == "shared" && !pol.allow_external_triggers {
                    return Err(Error::rejected(
                        "external_trigger_refused: shared workspace connection does not allow incoming external event triggers",
                    ));
                }
            }
            for pol in &all_policies {
                if !candidate_providers.is_empty()
                    && !candidate_providers.iter().any(|p| p == &pol.provider)
                {
                    continue;
                }
                if candidate_providers.is_empty() {
                    continue;
                }
                if pol.visibility == "shared" && !pol.allow_external_triggers {
                    return Err(Error::rejected(
                        "external_trigger_refused: shared workspace connection does not allow incoming external event triggers",
                    ));
                }
            }
        }

        Ok(None)
    }

    pub(super) fn execute_routine_gated(
        self: &Arc<Self>,
        id: &str,
        due_instant: i64,
        now_epoch: i64,
        trigger_source: &str,
    ) -> Result<Value> {
        let routine = self.store.routine_get(id)?;

        // 1. Destination thread live check before any effect (persisting
        //    `repoint_required` so scheduled ticks do not hot-loop).
        if !self
            .store
            .destination_thread_live(routine.destination_thread.as_deref())?
        {
            let _ = self.store.routine_mark_repoint_required(id);
            return Err(Error::rejected(
                "repoint_required: destination thread is missing or archived",
            ));
        }

        // 2. Paused check before any effect.
        if routine.status == "paused" {
            return Err(Error::rejected("routine is paused"));
        }

        // 3. Connection policy & untrusted inbound event isolation on shared workspace connections.
        let resolved_connection =
            self.verify_routine_connection_policy(&routine, trigger_source)?;

        // 4. Live Markdown revision gate (zero run/script/queue side effects on mismatch;
        //    marks `waiting_approval` in SQLite so the scheduler stops polling).
        let verified_markdown = if routine.requires_markdown() {
            let approved_rev = routine.approved_rev.as_deref().ok_or_else(|| {
                let _ = self.store.routine_mark_approval_drift(id);
                Error::rejected(
                    "unapproved_revision: routine markdown target requires owner approval",
                )
            })?;
            let md_path = routine.markdown_path.as_deref().unwrap_or("");
            let (text, live_rev) = self.resolve_routine_markdown(md_path)?;
            if live_rev != approved_rev {
                let _ = self.store.routine_mark_approval_drift(id);
                return Err(Error::rejected(format!(
                    "unapproved_revision: markdown target changed (approved '{approved_rev}', live '{live_rev}'); owner re-approval required"
                )));
            }
            Some(text)
        } else {
            None
        };

        // 5. Live Script SHA-256 digest gate (zero run/script/queue side effects on mismatch —
        //    a modified script is NEVER spawned and records no run row).
        let verified_script = if routine.requires_script() {
            let approved_sha = routine.approved_sha256.as_deref().ok_or_else(|| {
                let _ = self.store.routine_mark_approval_drift(id);
                Error::rejected("unapproved_digest: routine script target requires owner approval")
            })?;
            let sp_path = routine.script_path.as_deref().unwrap_or("");
            let (bytes, live_sha) = self.resolve_routine_script(sp_path)?;
            if !live_sha.eq_ignore_ascii_case(approved_sha) {
                let _ = self.store.routine_mark_approval_drift(id);
                return Err(Error::rejected(format!(
                    "unapproved_digest: script digest changed (approved '{approved_sha}', live '{live_sha}'); owner re-approval required"
                )));
            }
            Some(bytes)
        } else {
            None
        };

        // 6. Deterministic slot admission (idempotency, 10m stale skip, overlap guard).
        let run = match self
            .store
            .routine_admit_slot(id, due_instant, now_epoch, trigger_source)?
        {
            SlotAdmission::Duplicate(existing) => {
                return Ok(json!({ "ok": true, "run": existing.to_json(true) }))
            }
            SlotAdmission::SkippedStale(skipped) | SlotAdmission::SkippedOverlap(skipped) => {
                return Ok(json!({ "ok": true, "run": skipped.to_json(false) }))
            }
            SlotAdmission::RepointRequired => {
                return Err(Error::rejected(
                    "repoint_required: destination thread is missing or archived",
                ))
            }
            SlotAdmission::Started(started) => started,
        };

        // 7. Execute verified target(s).
        let mut script_output: Option<Value> = None;
        if let Some(ref script_bytes) = verified_script {
            match self.execute_verified_script(&routine.id, due_instant, script_bytes) {
                Ok((stdout, exit_ten_empty)) => {
                    let (has_items, parsed) = classify_script_stdout(&stdout);
                    script_output = parsed;
                    if exit_ten_empty || !has_items {
                        let finished = self.store.routine_finish_run(
                            &run.id,
                            RunOutcome::SkippedEmpty {
                                script_output: script_output.clone(),
                            },
                        )?;
                        self.wake();
                        return Ok(json!({
                            "ok": true,
                            "run": finished.to_json(false),
                        }));
                    }
                }
                Err(err_msg) => {
                    let failed = self
                        .store
                        .routine_finish_run(&run.id, RunOutcome::Failed { error: err_msg })?;
                    self.wake();
                    return Ok(json!({
                        "ok": false,
                        "run": failed.to_json(false),
                    }));
                }
            }
        }

        let mut message_id: Option<String> = None;
        if let Some(ref md_text) = verified_markdown {
            let agent_row = match self.store.agent(&routine.agent_alias) {
                Ok(a) => a,
                Err(e) => {
                    let failed = self.store.routine_finish_run(
                        &run.id,
                        RunOutcome::Failed {
                            error: format!(
                                "target agent '{}' is unavailable: {e}",
                                routine.agent_alias
                            ),
                        },
                    )?;
                    self.wake();
                    return Ok(json!({
                        "ok": false,
                        "run": failed.to_json(false),
                    }));
                }
            };
            let mut prompt = format!(
                "# Routine: {}\nTarget: `{}`\n",
                routine.name,
                routine.markdown_path.as_deref().unwrap_or(""),
            );
            if let Some((ref prov, ref acct)) = resolved_connection {
                if prov.is_empty() {
                    prompt.push_str(&format!("Connection account: `{acct}`\n"));
                } else {
                    prompt.push_str(&format!("Connection account: `{prov}/{acct}`\n"));
                }
            }
            prompt.push('\n');
            prompt.push_str(md_text);
            if let Some(ref out) = script_output {
                prompt.push_str("\n\n## Script Pre-check Output\n```json\n");
                prompt.push_str(
                    &serde_json::to_string_pretty(out).unwrap_or_else(|_| "{}".to_string()),
                );
                prompt.push_str("\n```\n");
            }
            let mid = format!("msg-rtn-{}", uuid::Uuid::new_v4().simple());
            match self.store.enqueue_steered_for_with_conversation(
                &routine.agent_alias,
                Some(&agent_row.uid),
                &prompt,
                None,
                &mid,
                "user",
                None,
                None,
                None,
                &crate::store::Sender::Operator,
                &crate::store::Steer::NONE,
                None,
                None,
                None,
                routine.destination_thread.as_deref(),
                None,
                None,
                None,
                None,
                None,
            ) {
                Ok(_) => {
                    self.notify_agent(&routine.agent_alias);
                    message_id = Some(mid);
                }
                Err(e) => {
                    let failed = self.store.routine_finish_run(
                        &run.id,
                        RunOutcome::Failed {
                            error: format!("failed to enqueue routine turn: {e}"),
                        },
                    )?;
                    self.wake();
                    return Ok(json!({
                        "ok": false,
                        "run": failed.to_json(false),
                    }));
                }
            }
        }

        let checklist_state = routine
            .checklist
            .iter()
            .map(|step| ChecklistStep {
                id: step.id.clone(),
                label: step.label.clone(),
                requires_approval: step.requires_approval,
                state: if step.requires_approval {
                    "waiting_approval".to_string()
                } else {
                    "completed".to_string()
                },
            })
            .collect();

        let finished = self.store.routine_finish_run(
            &run.id,
            RunOutcome::Completed {
                message_id,
                script_output,
                checklist_state,
            },
        )?;
        self.wake();
        Ok(json!({
            "ok": true,
            "run": finished.to_json(false),
        }))
    }

    pub(super) fn rpc_routine_run_decide(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("routine run decide", params, peer_pid)?;
        let run_id = params
            .get("run_id")
            .or_else(|| params.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| Error::rejected("routine run decide requires 'run_id'"))?;
        let decision = required_str(params, "decision")?;
        let approve = match decision {
            "approve" | "approved" => true,
            "dismiss" | "dismissed" | "reject" | "rejected" => false,
            _ => {
                return Err(Error::rejected(
                    "routine run decision must be 'approve' or 'dismiss'",
                ))
            }
        };

        let existing_run = self.store.routine_run_get(run_id)?;
        if existing_run.state != "waiting_approval" {
            return Err(Error::rejected(format!(
                "routine run '{run_id}' is '{}' and is not waiting for approval",
                existing_run.state
            )));
        }
        let routine = self.store.routine_get(&existing_run.routine_id)?;

        let mut follow_up_msg: Option<String> = None;
        if approve {
            let agent_row = self.store.agent(&routine.agent_alias)?;
            let step_labels = existing_run
                .checklist_state
                .iter()
                .filter(|s| s.state == "waiting_approval" || s.requires_approval)
                .map(|s| s.label.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let body = format!(
                "Owner approved routine '{}' step(s): {}. Proceed with the approved action.",
                routine.name,
                if step_labels.is_empty() {
                    "external action"
                } else {
                    &step_labels
                }
            );
            let conv = routine
                .destination_thread
                .as_deref()
                .filter(|t| self.store.destination_thread_live(Some(t)).unwrap_or(false));
            let mid = format!("msg-rtnstep-{}", uuid::Uuid::new_v4().simple());
            self.store.enqueue_steered_for_with_conversation(
                &routine.agent_alias,
                Some(&agent_row.uid),
                &body,
                None,
                &mid,
                "user",
                None,
                None,
                None,
                &crate::store::Sender::Operator,
                &crate::store::Steer::NONE,
                None,
                None,
                None,
                conv,
                None,
                None,
                None,
                None,
                None,
            )?;
            self.notify_agent(&routine.agent_alias);
            follow_up_msg = Some(mid);
        }

        let updated = self
            .store
            .routine_run_decide(run_id, decision, follow_up_msg.as_deref())?;
        self.wake();
        Ok(json!({
            "ok": true,
            "run": updated.to_json(false),
        }))
    }

    pub(super) fn rpc_routine_proposal_decide(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("routine proposal decide", params, peer_pid)?;
        let proposal_id = params
            .get("proposal_id")
            .or_else(|| params.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| Error::rejected("routine proposal decide requires 'proposal_id'"))?;
        let decision = required_str(params, "decision")?;
        let approve = match decision {
            "approve" | "approved" => true,
            "dismiss" | "dismissed" | "reject" | "rejected" => false,
            _ => {
                return Err(Error::rejected(
                    "proposal decision must be 'approve' or 'dismiss'",
                ))
            }
        };

        let existing = self.store.staged_proposal_get(proposal_id)?;
        if existing.state != "pending" {
            return Err(Error::rejected(format!(
                "staged proposal '{proposal_id}' is already '{}'",
                existing.state
            )));
        }

        if !approve {
            let decided = self.store.staged_proposal_decide(proposal_id, "dismiss")?;
            self.wake();
            return Ok(json!({
                "ok": true,
                "proposal": decided.to_json(),
            }));
        }

        match existing.kind.as_str() {
            "wiki_propose" => {
                let path = existing
                    .payload
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::rejected("wiki_propose payload is missing 'path'"))?;
                let text = existing
                    .payload
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::rejected("wiki_propose payload is missing 'text'"))?;
                let pm = self.pm()?;
                let write_out =
                    crate::wiki::write(&pm, &crate::wiki::Caller::Operator, path, text, None)?;
                if write_out["committed"] == true {
                    self.wiki_index.kick();
                }
                let decided = self.store.staged_proposal_decide(proposal_id, "approve")?;
                self.wake();
                Ok(json!({
                    "ok": true,
                    "proposal": decided.to_json(),
                    "wiki": write_out,
                }))
            }
            "routine_propose" => {
                let payload = &existing.payload;
                let name = payload
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&existing.title)
                    .to_string();
                let agent_alias = payload
                    .get("agent_alias")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        Error::rejected("routine_propose payload is missing 'agent_alias'")
                    })?
                    .to_string();
                let trigger_kind = payload
                    .get("trigger_kind")
                    .and_then(Value::as_str)
                    .unwrap_or("schedule")
                    .to_string();
                let cron = payload
                    .get("cron")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let interval_mins = payload.get("interval_mins").and_then(Value::as_i64);
                let timezone = payload
                    .get("timezone")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let event_source = payload
                    .get("event_source")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let target_kind = payload
                    .get("target_kind")
                    .and_then(Value::as_str)
                    .unwrap_or("markdown")
                    .to_string();
                let markdown_path = payload
                    .get("markdown_path")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let script_path = payload
                    .get("script_path")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let checklist: Vec<ChecklistStep> = payload
                    .get("checklist")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|e| Error::rejected(format!("invalid proposal checklist: {e}")))?
                    .unwrap_or_default();

                let mut approved_rev: Option<String> = None;
                let mut approved_sha: Option<String> = None;
                if matches!(target_kind.as_str(), "markdown" | "script_then_markdown") {
                    let md = markdown_path.as_deref().ok_or_else(|| {
                        Error::rejected("routine_propose requires 'markdown_path'")
                    })?;
                    let (_, live_rev) = self.resolve_routine_markdown(md)?;
                    approved_rev = Some(live_rev);
                }
                if matches!(target_kind.as_str(), "script" | "script_then_markdown") {
                    let sp = script_path
                        .as_deref()
                        .ok_or_else(|| Error::rejected("routine_propose requires 'script_path'"))?;
                    let (_, live_sha) = self.resolve_routine_script(sp)?;
                    approved_sha = Some(live_sha);
                }

                let now_epoch = crate::issue::time::now_epoch();
                let created = self.store.routine_create(RoutineCreate {
                    name,
                    agent_alias,
                    trigger_kind,
                    cron,
                    interval_mins,
                    timezone,
                    event_source,
                    target_kind,
                    markdown_path,
                    script_path,
                    destination_thread: None,
                    connection_account: None,
                    checklist,
                    inputs: json!({}),
                    now_epoch,
                })?;
                let activated = self.store.routine_approve(
                    &created.id,
                    approved_rev,
                    approved_sha,
                    now_epoch,
                )?;
                let decided = self.store.staged_proposal_decide(proposal_id, "approve")?;
                self.wake();
                Ok(json!({
                    "ok": true,
                    "proposal": decided.to_json(),
                    "routine": self.project_routine(&activated)?,
                }))
            }
            other => Err(Error::rejected(format!(
                "unsupported proposal kind '{other}'"
            ))),
        }
    }

    pub(super) fn rpc_routine_pause(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("routine pause", params, peer_pid)?;
        let id = required_str(params, "id")?;
        let reason = optional_str(params, "reason");
        let paused = self.store.routine_pause(id, reason)?;
        self.wake();
        Ok(json!({
            "ok": true,
            "routine": self.project_routine(&paused)?,
        }))
    }

    pub(super) fn rpc_routine_resume(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("routine resume", params, peer_pid)?;
        let id = required_str(params, "id")?;
        let resumed = self
            .store
            .routine_resume(id, crate::issue::time::now_epoch())?;
        self.wake();
        Ok(json!({
            "ok": true,
            "routine": self.project_routine(&resumed)?,
        }))
    }

    pub(super) fn rpc_routine_remove(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("routine remove", params, peer_pid)?;
        let id = required_str(params, "id")?;
        let removed = self.store.routine_remove(id)?;
        self.wake();
        Ok(json!({
            "ok": true,
            "removed": removed.id,
        }))
    }

    pub(super) fn rpc_wiki_propose(&self, params: &Value) -> Result<Value> {
        let proposed_by = optional_str(params, "by").unwrap_or("operator");
        let path = required_str(params, "path")?;
        let norm = crate::wiki::normalize(path)?;
        if norm.is_empty() || !norm.ends_with(".md") {
            return Err(Error::rejected(
                "wiki_propose requires a normalized '.md' wiki path",
            ));
        }
        let text = required_str(params, "text")?;
        let title = optional_str(params, "title")
            .map(str::to_string)
            .unwrap_or_else(|| format!("Playbook draft: wiki:{norm}"));
        let rev = crate::wiki::rev_bytes(text.as_bytes());
        let proposal = self.store.staged_proposal_create(
            "wiki_propose",
            proposed_by,
            &title,
            &json!({
                "path": norm,
                "markdown_path": format!("wiki:{norm}"),
                "text": text,
                "proposed_rev": rev,
            }),
        )?;
        self.wake();
        Ok(json!({
            "ok": true,
            "proposal": proposal.to_json(),
        }))
    }

    pub(super) fn rpc_routine_propose(&self, params: &Value) -> Result<Value> {
        let proposed_by = optional_str(params, "by").unwrap_or("operator");
        let name = required_str(params, "name")?;
        let agent_alias = params
            .get("agent_alias")
            .or_else(|| params.get("agent"))
            .and_then(Value::as_str)
            .ok_or_else(|| Error::rejected("routine_propose requires 'agent_alias'"))?;
        crate::proto::identifier(agent_alias, "Agent alias")?;
        let trigger_kind = optional_str(params, "trigger_kind").unwrap_or("schedule");
        if trigger_kind == "schedule" {
            let tz = optional_str(params, "timezone").ok_or_else(|| {
                Error::rejected("routine_propose schedule requires explicit IANA 'timezone'")
            })?;
            crate::store::routines::parse_iana_timezone(tz)?;
            let _ = crate::store::routines::next_due_instant(
                optional_str(params, "cron"),
                params.get("interval_mins").and_then(Value::as_i64),
                tz,
                crate::issue::time::now_epoch(),
            )?;
        }
        let proposal = self.store.staged_proposal_create(
            "routine_propose",
            proposed_by,
            name,
            &json!({
                "name": name,
                "agent_alias": agent_alias,
                "trigger_kind": trigger_kind,
                "cron": params.get("cron").cloned().unwrap_or(Value::Null),
                "interval_mins": params.get("interval_mins").cloned().unwrap_or(Value::Null),
                "timezone": params.get("timezone").cloned().unwrap_or(Value::Null),
                "event_source": params.get("event_source").cloned().unwrap_or(Value::Null),
                "target_kind": optional_str(params, "target_kind").unwrap_or("markdown"),
                "markdown_path": params.get("markdown_path").cloned().unwrap_or(Value::Null),
                "script_path": params.get("script_path").cloned().unwrap_or(Value::Null),
                "checklist": parse_checklist(params)?,
            }),
        )?;
        self.wake();
        Ok(json!({
            "ok": true,
            "proposal": proposal.to_json(),
        }))
    }

    pub(super) fn rpc_connection_policy_get(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("connection policy get", params, peer_pid)?;
        let id = params
            .get("connection_id")
            .or_else(|| params.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| Error::rejected("connection policy get requires 'connection_id'"))?;
        let cred = self.store.connection_credential(id)?;
        let fallback_provider = cred.as_ref().map(|r| r.platform.as_str()).unwrap_or("");
        let account = cred.as_ref().map(|r| r.account.as_str()).unwrap_or("");
        let policy = self
            .store
            .connection_policy_resolve(id, fallback_provider, account)?;
        Ok(json!({ "policy": policy.to_json() }))
    }

    pub(super) fn rpc_connection_policy_set(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("connection policy set", params, peer_pid)?;
        let id = params
            .get("connection_id")
            .or_else(|| params.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| Error::rejected("connection policy set requires 'connection_id'"))?;
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(Error::rejected("connection ID is invalid"));
        }

        let visibility = required_str(params, "visibility")?.to_string();
        let write_mode = optional_str(params, "write_mode")
            .unwrap_or("ask_approval")
            .to_string();
        let allow_external_triggers = params
            .get("allow_external_triggers")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let is_default = params
            .get("is_default")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let owner_user_id = optional_str(params, "owner_user_id").map(str::to_string);
        let allowed_agents = parse_allowed_agents(params)?;
        let expected_policy_revision = params
            .get("expected_policy_revision")
            .and_then(Value::as_i64);

        let existing_cred = self.store.connection_credential(id)?;
        let provider = existing_cred
            .as_ref()
            .map(|r| r.platform.clone())
            .or_else(|| optional_str(params, "provider").map(str::to_string))
            .unwrap_or_default();
        let account = existing_cred
            .as_ref()
            .map(|r| r.account.as_str())
            .unwrap_or("");
        let existing_policy = self
            .store
            .connection_policy_resolve(id, &provider, account)?;

        // CAD-2317 Acceptance #4: Switching a shared connection to Personal
        // MUST clear/revoke the shared credential first before applying personal mode.
        let mut shared_credential_cleared = false;
        if existing_policy.visibility == "shared" && visibility == "personal" {
            if let Some(ref record) = existing_cred {
                self.revoke_inner_expected(
                    &json!({
                        "platform": record.platform,
                        "account": record.account,
                    }),
                    Some(id),
                    Some(record.credential_revision),
                )?;
                shared_credential_cleared = true;
            }
        }

        let saved = self.store.connection_policy_set(ConnectionPolicyInput {
            connection_id: id.to_string(),
            provider: provider.clone(),
            visibility: visibility.clone(),
            owner_user_id: owner_user_id.clone(),
            is_default,
            write_mode: write_mode.clone(),
            allow_external_triggers,
            allowed_agents: allowed_agents.clone(),
            expected_policy_revision,
        })?;

        if let Some(ref record) = existing_cred {
            if shared_credential_cleared || visibility == "shared" {
                let _ = self.store.connection_policy_set(ConnectionPolicyInput {
                    connection_id: format!("acct:{}:{}", record.platform, record.account),
                    provider,
                    visibility,
                    owner_user_id,
                    is_default,
                    write_mode,
                    allow_external_triggers,
                    allowed_agents,
                    expected_policy_revision: None,
                });
            }
        }
        self.wake();
        Ok(json!({
            "ok": true,
            "policy": saved.to_json(),
            "shared_credential_cleared": shared_credential_cleared,
        }))
    }

    pub(super) fn rpc_credential_request_continue(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("credential request continue", params, peer_pid)?;
        let request_id = required_str(params, "request_id")?;
        if self
            .store
            .credential_request_resolution(request_id)?
            .is_some()
        {
            return Err(Error::rejected(
                "credential_request_already_resumed: paused reply was already continued",
            ));
        }

        let request = self
            .store
            .credential_request(request_id)?
            .ok_or_else(|| Error::rejected("credential request is unavailable"))?;

        let chosen_connection_id = optional_str(params, "connection_id")
            .map(str::to_string)
            .or_else(|| request.result_connection_id.clone())
            .or_else(|| request.connection_id.clone())
            .ok_or_else(|| {
                Error::rejected(
                    "credential request has no active connection account to resume with",
                )
            })?;

        let override_supplied = optional_str(params, "connection_id").is_some();
        let request_ready =
            request.state == "done" && matches!(request.status.as_str(), "saved" | "verified");
        if !override_supplied && !request_ready {
            return Err(Error::rejected(
                "reconnect is not complete yet; complete sign-in or choose another connected account",
            ));
        }

        // Verify live enrolled credential + unlocked custody readiness (`CAD-1085`).
        let cred = self
            .store
            .connection_credential(&chosen_connection_id)?
            .ok_or_else(|| Error::rejected("selected connection is unavailable or revoked"))?;
        if cred.platform != request.provider {
            return Err(Error::rejected(
                "selected connection belongs to a different provider",
            ));
        }
        if !override_supplied
            && cred
                .custody
                .starts_with(crate::platform::encrypted_sqlite::TAG_PREFIX)
            && !self.protected_credential_request_ready(&request)
        {
            return Err(Error::rejected(
                "custody_not_ready: credential custody key is locked or protected context changed",
            ));
        }
        {
            let _admission = self.custody_operation()?;
            crate::platform::load_credential(
                &self.store,
                &self.platform_custody,
                &cred.platform,
                &cred.account,
            )
            .map_err(|_| {
                Error::rejected("custody_not_ready: credential custody is locked or unavailable")
            })?;
        }

        let agents = self.store.agents()?;
        let target_agent = agents
            .iter()
            .find(|a| {
                a.archived_at.is_none()
                    && (Some(a.uid.as_str()) == request.target_agent_uid.as_deref()
                        || Some(a.uid.as_str()) == request.requester_participant.as_deref())
            })
            .cloned()
            .or_else(|| {
                self.store
                    .agent("master")
                    .ok()
                    .filter(|a| a.archived_at.is_none())
            })
            .ok_or_else(|| {
                Error::rejected(
                    "target agent for paused conversation is unavailable; cannot continue reply",
                )
            })?;

        let mid = format!("msg-resume-{}", uuid::Uuid::new_v4().simple());
        let body = format!(
            "Connection '{}' ({} / {}) is ready. Continue your paused reply.",
            request.provider, cred.account, chosen_connection_id
        );
        let conv = request.conversation_id.as_deref();
        self.store.enqueue_steered_for_with_conversation(
            &target_agent.alias,
            Some(&target_agent.uid),
            &body,
            None,
            &mid,
            "user",
            None,
            None,
            None,
            &crate::store::Sender::Operator,
            &crate::store::Steer::NONE,
            None,
            None,
            None,
            conv,
            None,
            None,
            None,
            None,
            None,
        )?;
        self.notify_agent(&target_agent.alias);
        let resolution = self.store.record_credential_request_resolution(
            request_id,
            &chosen_connection_id,
            &mid,
            conv,
        )?;
        self.wake();
        Ok(json!({
            "ok": true,
            "request_id": resolution.request_id,
            "connection_id": resolution.connection_id,
            "resumed": true,
            "message_id": resolution.message_id,
            "conversation_id": resolution.conversation_id,
        }))
    }

    pub(super) fn run_due_routines_tick(self: &Arc<Self>) {
        let now_epoch = crate::issue::time::now_epoch();
        let Ok(due) = self.store.routines_due(now_epoch) else {
            return;
        };
        for routine in due {
            let Some(slot) = routine.next_due_at else {
                continue;
            };
            let _ = self.execute_routine_gated(&routine.id, slot, now_epoch, "schedule");
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn protected_credential_request_ready(
        &self,
        request: &crate::store::credential_requests::CredentialRequest,
    ) -> bool {
        super::credential_entry_rpc::credential_request_ready(self, request)
    }

    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    fn protected_credential_request_ready(
        &self,
        _request: &crate::store::credential_requests::CredentialRequest,
    ) -> bool {
        false
    }
}
