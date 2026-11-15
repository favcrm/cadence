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

use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
        let full = if path.is_absolute() {
            let pm_canon = pm.dir.canonicalize()?;
            let state_canon = self.state_dir.canonicalize()?;
            let meta = std::fs::symlink_metadata(path)
                .map_err(|_| Error::rejected(format!("script target '{raw}' does not exist")))?;
            if meta.file_type().is_symlink() {
                return Err(Error::rejected(
                    "symlinks are not permitted in routine script_path",
                ));
            }
            let canon = path
                .canonicalize()
                .map_err(|_| Error::rejected(format!("script target '{raw}' does not exist")))?;
            if !canon.starts_with(&pm_canon) && !canon.starts_with(&state_canon) {
                return Err(Error::rejected(
                    "script_path must reside inside the workspace tracker or state directory",
                ));
            }
            canon
        } else {
            ensure_no_symlinks_under(&pm.dir, path)?
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
    ) -> std::result::Result<Vec<u8>, String> {
        let exec_dir = self.state_dir.join("routine-exec");
        if let Err(e) = std::fs::create_dir_all(&exec_dir) {
            return Err(format!("failed to create routine-exec dir: {e}"));
        }
        let _ = std::fs::set_permissions(&exec_dir, std::fs::Permissions::from_mode(0o700));
        let tmp_script = exec_dir.join(format!("run-{}.bin", uuid::Uuid::new_v4().simple()));
        {
            use std::io::Write;
            let mut file = match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o700)
                .open(&tmp_script)
            {
                Ok(f) => f,
                Err(e) => return Err(format!("failed to stage verified script: {e}")),
            };
            if let Err(e) = file
                .write_all(verified_bytes)
                .and_then(|()| file.sync_all())
            {
                let _ = std::fs::remove_file(&tmp_script);
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
        cmd.current_dir(&workdir)
            .env("CADENCE_ROUTINE_ID", routine_id)
            .env("CADENCE_ROUTINE_DUE", due_instant.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match crate::reaper::spawn(&mut cmd) {
            Ok(c) => c,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp_script);
                return Err(format!("failed to spawn routine script: {e}"));
            }
        };

        let stdout_handle = child.stdout.take().map(|mut out| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = (&mut out)
                    .take(MAX_SCRIPT_OUTPUT_BYTES as u64)
                    .read_to_end(&mut buf);
                let _ = std::io::copy(&mut out, &mut std::io::sink());
                buf
            })
        });
        let stderr_handle = child.stderr.take().map(|mut err| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = (&mut err).take(4096).read_to_end(&mut buf);
                let _ = std::io::copy(&mut err, &mut std::io::sink());
                buf
            })
        });

        let deadline = Instant::now() + SCRIPT_TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(st)) => break st,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = std::fs::remove_file(&tmp_script);
                        if let Some(h) = stdout_handle {
                            let _ = h.join();
                        }
                        if let Some(h) = stderr_handle {
                            let _ = h.join();
                        }
                        return Err("routine script timed out after 30s".to_string());
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = std::fs::remove_file(&tmp_script);
                    if let Some(h) = stdout_handle {
                        let _ = h.join();
                    }
                    if let Some(h) = stderr_handle {
                        let _ = h.join();
                    }
                    return Err(format!("failed waiting for routine script: {e}"));
                }
            }
        };
        let _ = std::fs::remove_file(&tmp_script);

        let stdout = stdout_handle
            .and_then(|h| h.join().ok())
            .unwrap_or_default();
        let stderr = stderr_handle
            .and_then(|h| h.join().ok())
            .unwrap_or_default();
        if !status.success() {
            let err_text = String::from_utf8_lossy(&stderr).trim().to_string();
            let detail = if err_text.is_empty() {
                format!("script exited with status {status}")
            } else {
                format!("script exited with status {status}: {err_text}")
            };
            return Err(detail);
        }
        Ok(stdout)
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

    pub(super) fn rpc_routine_list(&self, params: &Value) -> Result<Value> {
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
        Ok(json!({
            "routines": routines,
            "staged_proposals": proposals,
        }))
    }

    pub(super) fn rpc_routine_show(&self, params: &Value) -> Result<Value> {
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
        let timezone = optional_str(params, "timezone").map(str::to_string);
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

    pub(super) fn execute_routine_gated(
        self: &Arc<Self>,
        id: &str,
        due_instant: i64,
        now_epoch: i64,
        trigger_source: &str,
    ) -> Result<Value> {
        let routine = self.store.routine_get(id)?;

        // 1. Destination thread live check before any effect.
        if !self
            .store
            .destination_thread_live(routine.destination_thread.as_deref())?
        {
            return Err(Error::rejected(
                "repoint_required: destination thread is missing or archived",
            ));
        }

        // 2. Paused check before any effect.
        if routine.status == "paused" {
            return Err(Error::rejected("routine is paused"));
        }

        // 3. Untrusted inbound event isolation on shared workspace connections.
        if (trigger_source == "external_event"
            || (routine.trigger_kind == "event" && trigger_source != "manual"))
            && routine.connection_account.is_some()
        {
            let conn_id = routine.connection_account.as_deref().unwrap_or("");
            let policy = self.store.connection_policy_get(conn_id, "")?;
            if policy.visibility == "shared" && !policy.allow_external_triggers {
                return Err(Error::rejected(
                    "external_trigger_refused: shared workspace connection does not allow incoming external event triggers",
                ));
            }
        }

        // 4. Live Markdown revision gate (zero side effects on mismatch).
        let verified_markdown = if routine.requires_markdown() {
            let approved_rev = routine.approved_rev.as_deref().ok_or_else(|| {
                Error::rejected(
                    "unapproved_revision: routine markdown target requires owner approval",
                )
            })?;
            let md_path = routine.markdown_path.as_deref().unwrap_or("");
            let (text, live_rev) = self.resolve_routine_markdown(md_path)?;
            if live_rev != approved_rev {
                return Err(Error::rejected(format!(
                    "unapproved_revision: markdown target changed (approved '{approved_rev}', live '{live_rev}'); owner re-approval required"
                )));
            }
            Some(text)
        } else {
            None
        };

        // 5. Live Script SHA-256 digest gate (zero side effects on mismatch —
        //    a modified script is NEVER spawned and records no run row).
        let verified_script = if routine.requires_script() {
            let approved_sha = routine.approved_sha256.as_deref().ok_or_else(|| {
                Error::rejected("unapproved_digest: routine script target requires owner approval")
            })?;
            let sp_path = routine.script_path.as_deref().unwrap_or("");
            let (bytes, live_sha) = self.resolve_routine_script(sp_path)?;
            if !live_sha.eq_ignore_ascii_case(approved_sha) {
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
                Ok(stdout) => {
                    let (has_items, parsed) = classify_script_stdout(&stdout);
                    script_output = parsed;
                    if !has_items {
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
            if self.store.agent(&routine.agent_alias).is_ok() {
                let mut prompt = format!(
                    "# Routine: {}\nTarget: `{}`\n\n{}",
                    routine.name,
                    routine.markdown_path.as_deref().unwrap_or(""),
                    md_text
                );
                if let Some(ref out) = script_output {
                    prompt.push_str("\n\n## Script Pre-check Output\n```json\n");
                    prompt.push_str(
                        &serde_json::to_string_pretty(out).unwrap_or_else(|_| "{}".to_string()),
                    );
                    prompt.push_str("\n```\n");
                }
                let mid = format!("msg-rtn-{}", uuid::Uuid::new_v4().simple());
                if self
                    .store
                    .enqueue(&routine.agent_alias, &prompt, None, &mid, "user")
                    .is_ok()
                {
                    message_id = Some(mid);
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

    pub(super) fn rpc_connection_policy_get(&self, params: &Value) -> Result<Value> {
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

        // Verify either the request completed (`done` + `saved`/`verified`) or an explicit alternate enrolled connection was chosen.
        let override_supplied = optional_str(params, "connection_id").is_some();
        let request_ready =
            request.state == "done" && matches!(request.status.as_str(), "saved" | "verified");
        if !override_supplied && !request_ready {
            return Err(Error::rejected(
                "reconnect is not complete yet; complete sign-in or choose another connected account",
            ));
        }
        if override_supplied {
            let alt = self
                .store
                .connection_credential(&chosen_connection_id)?
                .ok_or_else(|| Error::rejected("selected alternate connection is unavailable"))?;
            if alt.platform != request.provider {
                return Err(Error::rejected(
                    "selected connection belongs to a different provider",
                ));
            }
        }

        let mut message_id: Option<String> = None;
        let target_alias = self
            .store
            .agents()?
            .into_iter()
            .find(|a| Some(a.uid.as_str()) == request.target_agent_uid.as_deref())
            .map(|a| a.alias)
            .or_else(|| self.store.agent("master").ok().map(|a| a.alias));

        if let Some(alias) = target_alias {
            let mid = format!("msg-resume-{}", uuid::Uuid::new_v4().simple());
            let body = format!(
                "Connection '{}' ({}) is ready. Continue your paused reply.",
                request.provider, chosen_connection_id
            );
            if self
                .store
                .enqueue(&alias, &body, None, &mid, "user")
                .is_ok()
            {
                message_id = Some(mid);
            }
        }
        self.wake();
        Ok(json!({
            "ok": true,
            "request_id": request_id,
            "connection_id": chosen_connection_id,
            "resumed": true,
            "message_id": message_id,
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
}
