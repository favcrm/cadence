//! Operator agents settings. The SQLite settings row is the authority for
//! model defaults, Pi policy, and profile text. `.pi/agents` and
//! `.claude/agents` under the PM root are generated exports.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::adapter::registry::{self, EndpointSpec};
use crate::error::{Error, Result};
use crate::issue::Pm;
use crate::master;
use crate::model_defaults::{
    validate_config, validate_model_id, ModelDefaults, ModelSelector, ProviderDefaults, TEAM_ROLES,
};
use crate::pi_policy::{self, PiPolicy};
use crate::store::{Agent, Store};

const PROFILE_MARKER: &str = "cadence_profile";
const GEN_MARKER: &str = "cadence-agent-profile";
const ACTOR: &str = "operator";

#[derive(Clone)]
struct Profile {
    id: String,
    role: String,
    model: String,
    job: String,
    text: String,
}

struct Overlay {
    harnesses: Vec<Value>,
    providers: Vec<Value>,
    models: Vec<Value>,
    scope: BTreeMap<String, String>,
    role_defaults: BTreeMap<String, String>,
    pins: Vec<Value>,
}

pub fn snapshot(pm_dir: &Path, store: &Store, agents: &[Agent]) -> Result<Value> {
    let current = store.settings()?;
    let profiles = profiles_from(&current.doc.profiles)?;
    let policy = current.doc.pi.clone();
    let overlay = present_overlay(&current.doc.model_defaults, policy.as_ref());
    let token = revision_token(
        current.revision,
        &current.doc.model_defaults.agent_settings,
        &profiles,
        policy.as_ref(),
    );
    Ok(with_retired(
        build_snapshot(
            token,
            &overlay,
            &profiles,
            policy.as_ref(),
            agents,
            pm_dir,
            &store.settings_history(8)?,
        ),
        retired_pi(pm_dir, current.legacy_imported, &policy),
    ))
}

pub fn save(
    pm: &Pm,
    state_dir: &Path,
    store: &Store,
    agents: &[Agent],
    expected: &str,
    config: &Value,
) -> Result<Value> {
    let _ = crate::secret::guard("agents settings", &config.to_string())?;
    store.import_legacy_settings(&pm.dir)?;
    let overlay = parse_overlay(config)?;
    let profiles = parse_profiles(config, &overlay)?;
    let current = store.settings()?;
    let previous = profiles_from(&current.doc.profiles)?;
    let policy = current.doc.pi.clone();
    let token = revision_token(
        current.revision,
        &current.doc.model_defaults.agent_settings,
        &previous,
        policy.as_ref(),
    );
    if token != expected {
        return Err(conflict(&token));
    }
    check_references(
        &present_overlay(&current.doc.model_defaults, policy.as_ref()),
        &overlay,
        &profiles,
        &previous,
        agents,
    )?;
    let mut next_defaults = project_defaults(&current.doc.model_defaults, &overlay)?;
    validate_config(&mut next_defaults)?;
    let _ = (pm, state_dir);
    let pi = project_pi(&policy, &overlay)?;
    let bodies = profiles
        .iter()
        .map(|profile| {
            let prior = previous.iter().find(|prev| prev.id == profile.id);
            if prior.is_none_or(|prev| prev.job != profile.job)
                && profile.job.len() > crate::store::MAX_INSTRUCTIONS_BYTES
            {
                return Err(Error::invalid(
                    "invalid_config",
                    format!(
                        "profile '{}' job must be at most {} UTF-8 bytes for launch",
                        profile.id,
                        crate::store::MAX_INSTRUCTIONS_BYTES,
                    ),
                ));
            }
            let text = prior
                .filter(|prev| {
                    prev.role == profile.role
                        && prev.model == profile.model
                        && prev.job == profile.job
                })
                .map(|prev| prev.text.clone())
                .unwrap_or_else(|| render_agent(profile));
            master::check_file("AGENT.md", &text)?;
            Ok(crate::store::ProfileBody {
                id: profile.id.clone(),
                text,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let saved = store.save_settings(
        current.revision,
        crate::store::SettingsDoc {
            model_defaults: next_defaults,
            pi,
            profiles: bodies,
        },
        ACTOR,
    )?;
    let profiles = profiles_from(&saved.doc.profiles)?;
    let token = revision_token(
        saved.revision,
        &saved.doc.model_defaults.agent_settings,
        &profiles,
        saved.doc.pi.as_ref(),
    );
    Ok(with_retired(
        build_snapshot(
            token,
            &present_overlay(&saved.doc.model_defaults, saved.doc.pi.as_ref()),
            &profiles,
            saved.doc.pi.as_ref(),
            agents,
            &pm.dir,
            &store.settings_history(8)?,
        ),
        retired_pi(&pm.dir, saved.legacy_imported, &saved.doc.pi),
    ))
}

/// `omp models <provider> --json --no-extensions`. The program and flags
/// are fixed. The provider id is one token, never a path or a flag, and
/// the child is not given an extension (`-e`) or a models.db refresh.
pub fn catalog(_pm_dir: &Path, provider: &str) -> Result<Value> {
    let source = format!("omp models {provider} --json --no-extensions");
    let fetched_at = iso_now();
    let err = |message: String| {
        json!({
            "source": source,
            "fetchedAt": fetched_at,
            "models": [],
            "error": message,
        })
    };
    if check_ident("provider", provider).is_err() || provider.starts_with('-') {
        return Ok(err(format!(
            "no supported native catalog for provider '{provider}'"
        )));
    }
    let program = match crate::adapter::pty::resolve_on_path("omp") {
        Ok(path) => path,
        Err(_) => {
            return Ok(err(
                "no supported native catalog: omp is not on PATH".to_string()
            ))
        }
    };
    let mut cmd = Command::new(program);
    cmd.args(["models", provider, "--json", "--no-extensions"])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for name in ["PATH", "HOME", "TMPDIR"] {
        if let Ok(value) = std::env::var(name) {
            cmd.env(name, value);
        }
    }
    let mut child = match crate::reaper::spawn(&mut cmd) {
        Ok(child) => child,
        Err(io) => return Ok(err(format!("omp models failed to start: {io}"))),
    };
    let fail = |child: &mut std::process::Child, message: &str| {
        let _ = child.kill();
        let _ = child.wait();
        Ok(err(message.to_string()))
    };
    let mut pipe = child.stdout.take().expect("catalog stdout was piped");
    let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return fail(&mut child, "catalog stdout could not be made nonblocking");
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut stdout = Vec::new();
    let mut chunk = [0; 16 * 1024];
    let status = loop {
        let exited = match child.try_wait() {
            Ok(exited) => exited,
            Err(_) => return fail(&mut child, "omp models status could not be read"),
        };
        loop {
            if Instant::now() >= deadline {
                return fail(&mut child, "omp models timed out");
            }
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(len) => {
                    if stdout.len() + len > 16 * 1024 * 1024 {
                        return fail(&mut child, "omp models output exceeds 16 MiB");
                    }
                    stdout.extend_from_slice(&chunk[..len]);
                }
                Err(io) if io.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(io) if io.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return fail(&mut child, "omp models stdout could not be read"),
            }
        }
        if let Some(status) = exited {
            break status;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if !status.success() {
        return Ok(err(format!("omp models exited {status}")));
    }
    let doc: Value = match serde_json::from_slice(&stdout) {
        Ok(doc) => doc,
        Err(parse) => return Ok(err(format!("omp models output is not JSON: {parse}"))),
    };
    let Some(rows) = doc.get("models").and_then(Value::as_array) else {
        return Ok(err("omp models output has no models array".to_string()));
    };
    let mut models = Vec::new();
    for row in rows {
        if row.get("kind").and_then(Value::as_str) != Some("chat")
            || row.get("provider").and_then(Value::as_str) != Some(provider)
        {
            continue;
        }
        let Some(id) = row.get("selector").and_then(Value::as_str) else {
            continue;
        };
        if id.is_empty() || id.chars().any(char::is_control) {
            continue;
        }
        models.push(json!({
            "id": id,
            "ctx": token_field(row, "contextWindow"),
            "out": token_field(row, "maxTokens"),
            "thinking": row.get("reasoning").and_then(Value::as_bool).map(|v| json!(v)).unwrap_or(Value::Null),
            "images": image_input(row),
        }));
    }
    if models.is_empty() {
        return Ok(err(format!(
            "omp models listed no chat selectors for provider '{provider}'"
        )));
    }
    Ok(json!({
        "source": source,
        "fetchedAt": fetched_at,
        "models": models,
    }))
}

pub fn sync(
    pm: &Pm,
    store: &Store,
    agents: &[Agent],
    expected: &str,
    profile: Option<&str>,
) -> Result<Value> {
    let current = store.settings()?;
    let profiles = profiles_from(&current.doc.profiles)?;
    let policy = current.doc.pi.clone();
    let token = revision_token(
        current.revision,
        &current.doc.model_defaults.agent_settings,
        &profiles,
        policy.as_ref(),
    );
    if token != expected {
        return Err(conflict(&token));
    }
    let selected: Vec<&Profile> = match profile {
        None => profiles.iter().collect(),
        Some(id) => {
            let found = profiles.iter().find(|p| p.id == id).ok_or_else(|| {
                Error::invalid("invalid_request", format!("unknown profile '{id}'"))
            })?;
            vec![found]
        }
    };
    if !selected.is_empty() {
        let root =
            open_export_root(&pm.dir)?.ok_or_else(|| Error::rejected("PM directory is missing"))?;
        for harness in ["pi", "claude"] {
            walk_export_dir(&root, harness, false)?;
        }
    }
    let mut files = Vec::new();
    for profile in selected {
        for harness in ["pi", "claude"] {
            let path = export_path(&pm.dir, harness, &profile.id);
            let status = export_status(&pm.dir, harness, &profile.id, profile);
            if status == "unmanaged" || status == "in sync" {
                files.push(json!({
                    "profile": profile.id,
                    "harness": harness,
                    "path": path.display().to_string(),
                    "status": status,
                }));
                continue;
            }
            write_export(&pm.dir, harness, &profile.id, profile)?;
            files.push(json!({
                "profile": profile.id,
                "harness": harness,
                "path": path.display().to_string(),
                "status": "in sync",
            }));
        }
    }
    let profiles = profiles_from(&current.doc.profiles)?;
    let token = revision_token(
        current.revision,
        &current.doc.model_defaults.agent_settings,
        &profiles,
        policy.as_ref(),
    );
    Ok(json!({
        "snapshot": build_snapshot(token, &present_overlay(&current.doc.model_defaults, policy.as_ref()), &profiles, policy.as_ref(), agents, &pm.dir, &store.settings_history(8)?),
        "files": files,
    }))
}

pub fn preview(store: &Store, expected: &str, profile_id: &str, harness_id: &str) -> Result<Value> {
    let current = store.settings()?;
    let profiles = profiles_from(&current.doc.profiles)?;
    let policy = current.doc.pi.clone();
    let token = revision_token(
        current.revision,
        &current.doc.model_defaults.agent_settings,
        &profiles,
        policy.as_ref(),
    );
    if token != expected {
        return Err(conflict(&token));
    }
    let overlay = present_overlay(&current.doc.model_defaults, policy.as_ref());
    let profile = profiles
        .iter()
        .find(|p| p.id == profile_id)
        .ok_or_else(|| {
            Error::invalid("invalid_request", format!("unknown profile '{profile_id}'"))
        })?;
    let harness = overlay
        .harnesses
        .iter()
        .find(|h| h["id"].as_str() == Some(harness_id))
        .ok_or_else(|| {
            Error::invalid("invalid_request", format!("unknown harness '{harness_id}'"))
        })?;
    let cli = harness["cli"].as_str().unwrap_or("");
    let spec = registry_cli(harness_id, cli);
    let mut reasons = Vec::new();
    if profile.job.len() > crate::store::MAX_INSTRUCTIONS_BYTES {
        reasons.push(format!(
            "profile job exceeds the {} UTF-8 byte launch limit",
            crate::store::MAX_INSTRUCTIONS_BYTES,
        ));
    }
    let mut model = resolve_model(profile, &overlay);
    if model.is_none() {
        if let Some(spec) = spec {
            let endpoint = registry::default_kind(spec.provider)?;
            let resolved = crate::model_defaults::resolve(crate::model_defaults::ResolveRequest {
                provider: spec.provider,
                endpoint_kind: endpoint,
                runtime_role: "worker",
                team_role: Some(&profile.role),
                model_policy: None,
                params: None,
                config: &current.doc.model_defaults,
                revision: current.revision,
            })?;
            if let Some(params) = resolved.params {
                let params: Value = serde_json::from_str(&params)?;
                if let Some(id) = params.get("model").and_then(Value::as_str) {
                    let source = resolved
                        .model_selection
                        .as_ref()
                        .and_then(|selection| selection.get("source"))
                        .and_then(Value::as_str)
                        .unwrap_or("provider_default");
                    model = Some((id.to_string(), source.to_string()));
                }
            }
            if spec.provider == "pi" {
                match pi_policy::resolve_model_with_source(
                    policy.as_ref(),
                    "worker",
                    model.as_ref().map(|(id, _)| id.as_str()),
                ) {
                    Ok((id, platform)) if model.is_none() => {
                        model = Some((
                            id,
                            if platform {
                                "platform_default"
                            } else {
                                "pi_policy_default"
                            }
                            .to_string(),
                        ))
                    }
                    Ok(_) => {}
                    Err(err) => reasons.push(err.to_string()),
                }
            }
        }
    }
    let (model_id, source) = match model {
        Some((id, source)) => (Some(id), source),
        None => (None, "provider_default".to_string()),
    };
    if spec.is_none() {
        reasons.push(format!(
            "harness cli '{cli}' is not a registered adapter for '{harness_id}'"
        ));
    }
    if let Some(id) = &model_id {
        reasons.extend(permission_reasons(
            id,
            &overlay,
            spec,
            policy.as_ref(),
            &profile.role,
        ));
    }
    if model_id.is_some()
        && spec.is_some_and(|item| {
            registry::default_kind(item.provider)
                .is_ok_and(|endpoint| !registry::supports_model(item.provider, endpoint))
        })
    {
        reasons.push("this harness does not accept a model".to_string());
    }
    if let Some(spec) = spec {
        let (installed, _) = probe_cli(spec, cli);
        if !installed {
            reasons.push(format!(
                "harness program '{}' is not installed",
                spec.probe_bins
                    .iter()
                    .find(|(p, _)| *p == cli)
                    .map(|(p, _)| *p)
                    .unwrap_or(cli)
            ));
        }
    }
    let command = if reasons.is_empty() {
        let provider = spec.map(|item| item.provider).unwrap_or(harness_id);
        let command = format!(
            "cadence join <pm> {} --profile {}",
            shell_quote(provider),
            shell_quote(profile_id),
        );
        Some(command)
    } else {
        None
    };
    Ok(json!({
        "command": command,
        "model": model_id,
        "source": source,
        "allowed": reasons.is_empty(),
        "reasons": reasons,
        "profile_revision": digest(&profile.text)[..12].to_string(),
    }))
}

fn conflict(token: &str) -> Error {
    Error::conflict(0, format!("settings revision is {token}"))
}

fn revision_token(
    revision: i64,
    overlay: &Value,
    profiles: &[Profile],
    policy: Option<&PiPolicy>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(canonical(overlay).as_bytes());
    hasher.update(b"\n");
    for profile in profiles {
        hasher.update(profile.id.as_bytes());
        hasher.update(b"\0");
        hasher.update(profile.text.as_bytes());
        hasher.update(b"\n");
    }
    hasher.update(pi_canonical(policy).as_bytes());
    let hash = hasher.finalize();
    let hex: String = hash.iter().take(6).map(|b| format!("{b:02x}")).collect();
    format!("{revision}:{hex}")
}

fn pi_canonical(policy: Option<&PiPolicy>) -> String {
    let Some(policy) = policy else {
        return "none".to_string();
    };
    let mut providers = policy.providers.clone();
    providers.sort();
    let mut allow = policy.models.allow.clone();
    allow.sort();
    let master = policy.models.master_allow.clone().map(|mut v| {
        v.sort();
        v
    });
    let worker = policy.models.worker_allow.clone().map(|mut v| {
        v.sort();
        v
    });
    json!({
        "providers": providers,
        "allow": allow,
        "master_allow": master,
        "worker_allow": worker,
        "default_master": policy.models.default.master,
        "default_worker": policy.models.default.worker,
    })
    .to_string()
}

fn canonical(value: &Value) -> String {
    fn write(out: &mut String, value: &Value) {
        match value {
            Value::Null => out.push_str("null"),
            Value::Bool(v) => out.push_str(if *v { "true" } else { "false" }),
            Value::Number(n) => out.push_str(&n.to_string()),
            Value::String(s) => {
                out.push('"');
                for ch in s.chars() {
                    match ch {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(out, item);
                }
                out.push(']');
            }
            Value::Object(map) => {
                out.push('{');
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                for (i, key) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(out, &Value::String((*key).clone()));
                    out.push(':');
                    write(out, &map[*key]);
                }
                out.push('}');
            }
        }
    }
    let mut out = String::new();
    write(&mut out, value);
    out
}

fn overlay_from(value: &Value) -> Overlay {
    let empty = Map::new();
    let obj = value.as_object().unwrap_or(&empty);
    let harnesses = obj
        .get("harnesses")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let providers = obj
        .get("providers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let models = obj
        .get("models")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut scope = BTreeMap::new();
    if let Some(map) = obj.get("scope").and_then(Value::as_object) {
        for (k, v) in map {
            if let Some(s) = v.as_str() {
                scope.insert(k.clone(), s.to_string());
            }
        }
    }
    let mut role_defaults = BTreeMap::new();
    if let Some(map) = obj.get("roleDefaults").and_then(Value::as_object) {
        for (k, v) in map {
            if let Some(s) = v.as_str() {
                role_defaults.insert(k.clone(), s.to_string());
            }
        }
    }
    let pins = obj
        .get("pins")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Overlay {
        harnesses,
        providers,
        models,
        scope,
        role_defaults,
        pins,
    }
}

fn parse_overlay(config: &Value) -> Result<Overlay> {
    let obj = config
        .as_object()
        .ok_or_else(|| Error::invalid("invalid_config", "config must be an object"))?;
    let harnesses = array_field(obj, "harnesses")?;
    let providers = array_field(obj, "providers")?;
    let models = array_field(obj, "models")?;
    let mut seen_h = BTreeSet::new();
    let mut norm_h = Vec::new();
    for harness in harnesses {
        let id = str_field(harness, "id")?;
        let label = str_field(harness, "label")?;
        let cli = str_field(harness, "cli")?;
        let note = str_field(harness, "note")?;
        check_short("harness label", &label, 120)?;
        check_short("harness note", &note, 400)?;
        if !seen_h.insert(id.clone()) {
            return Err(Error::invalid(
                "invalid_config",
                format!("duplicate harness '{id}'"),
            ));
        }
        if registry_cli(&id, &cli).is_none() {
            return Err(Error::invalid(
                "invalid_config",
                format!("harness '{id}' cli '{cli}' is not a registered adapter program"),
            ));
        }
        if cli.contains('/') || cli.contains('\\') {
            return Err(Error::invalid(
                "invalid_config",
                "harness cli must be a registry program name, not a path",
            ));
        }
        norm_h.push(json!({"id": id, "label": label, "cli": cli, "note": note}));
    }
    let mut seen_p = BTreeSet::new();
    let mut norm_p = Vec::new();
    for provider in providers {
        let id = str_field(provider, "id")?;
        check_ident("provider", &id)?;
        let label = str_field(provider, "label")?;
        let kind = str_field(provider, "kind")?;
        let pin = str_field(provider, "pin")?;
        let gate = bool_req(provider, "gate")?;
        let paid = bool_req(provider, "paid")?;
        let note = str_field(provider, "note")?;
        check_short("provider label", &label, 120)?;
        check_short("provider note", &note, 400)?;
        check_short("provider pin", &pin, 120)?;
        if !matches!(kind.as_str(), "Pi extension" | "native CLI") {
            return Err(Error::invalid(
                "invalid_config",
                "provider kind must be 'Pi extension' or 'native CLI'",
            ));
        }
        if (kind == "Pi extension") != gate {
            return Err(Error::invalid(
                "invalid_config",
                format!("provider '{id}' gate must be true only for a Pi extension"),
            ));
        }
        if !pin.is_empty() {
            parse_pin_text(&pin)?;
        }
        if !seen_p.insert(id.clone()) {
            return Err(Error::invalid(
                "invalid_config",
                format!("duplicate provider '{id}'"),
            ));
        }
        norm_p.push(json!({
            "id": id, "label": label, "kind": kind, "pin": pin, "gate": gate, "paid": paid, "note": note,
        }));
    }
    let mut seen_m = BTreeSet::new();
    let mut norm_m = Vec::new();
    for model in models {
        let id = validate_model_id(str_field(model, "id")?.as_str())?;
        let provider = str_field(model, "provider")?;
        if !seen_p.contains(&provider) {
            return Err(Error::invalid(
                "invalid_config",
                format!("model '{id}' names unknown provider '{provider}'"),
            ));
        }
        let ctx = str_field(model, "ctx")?;
        let out = str_field(model, "out")?;
        check_short("model ctx", &ctx, 32)?;
        check_short("model out", &out, 32)?;
        let thinking = tri_bool(model, "thinking")?;
        let images = tri_bool(model, "images")?;
        if !seen_m.insert(id.clone()) {
            return Err(Error::invalid(
                "invalid_config",
                format!("duplicate model '{id}'"),
            ));
        }
        norm_m.push(json!({
            "id": id, "provider": provider, "ctx": ctx, "out": out, "thinking": thinking, "images": images,
        }));
    }
    let scope_val = obj
        .get("scope")
        .ok_or_else(|| Error::invalid("invalid_config", "config.scope is required"))?;
    let scope_obj = scope_val
        .as_object()
        .ok_or_else(|| Error::invalid("invalid_config", "config.scope must be an object"))?;
    let mut scope = BTreeMap::new();
    for (key, value) in scope_obj {
        if !seen_m.contains(key) {
            continue;
        }
        let provider = norm_m
            .iter()
            .find(|row| row["id"].as_str() == Some(key.as_str()))
            .and_then(|row| row["provider"].as_str())
            .unwrap_or("");
        let kind = norm_p
            .iter()
            .find(|row| row["id"].as_str() == Some(provider))
            .and_then(|row| row["kind"].as_str())
            .unwrap_or("");
        if kind == "native CLI" {
            continue;
        }
        let mode = value.as_str().ok_or_else(|| {
            Error::invalid("invalid_config", format!("scope '{key}' must be a string"))
        })?;
        if !matches!(mode, "off" | "allow" | "master" | "worker") {
            return Err(Error::invalid(
                "invalid_config",
                format!("scope '{key}' must be off, allow, master, or worker"),
            ));
        }
        scope.insert(key.clone(), mode.to_string());
    }
    for id in &seen_m {
        if scope.contains_key(id) {
            continue;
        }
        let provider = norm_m
            .iter()
            .find(|row| row["id"].as_str() == Some(id.as_str()))
            .and_then(|row| row["provider"].as_str())
            .unwrap_or("");
        let kind = norm_p
            .iter()
            .find(|row| row["id"].as_str() == Some(provider))
            .and_then(|row| row["kind"].as_str())
            .unwrap_or("");
        if kind == "native CLI" {
            continue;
        }
        return Err(Error::invalid(
            "invalid_config",
            format!("model '{id}' needs a scope"),
        ));
    }
    let roles_val = obj
        .get("roleDefaults")
        .ok_or_else(|| Error::invalid("invalid_config", "config.roleDefaults is required"))?;
    let roles_obj = roles_val
        .as_object()
        .ok_or_else(|| Error::invalid("invalid_config", "config.roleDefaults must be an object"))?;
    let mut role_defaults = BTreeMap::new();
    for (role, _) in TEAM_ROLES {
        let value = roles_obj.get(*role).ok_or_else(|| {
            Error::invalid("invalid_config", format!("roleDefaults missing '{role}'"))
        })?;
        let model = value.as_str().ok_or_else(|| {
            Error::invalid(
                "invalid_config",
                format!("roleDefaults '{role}' must be a string"),
            )
        })?;
        if !model.is_empty() && !seen_m.contains(model) {
            return Err(Error::invalid(
                "invalid_config",
                format!("roleDefaults '{role}' names unknown model '{model}'"),
            ));
        }
        role_defaults.insert((*role).to_string(), model.to_string());
    }
    for key in roles_obj.keys() {
        if !TEAM_ROLES.iter().any(|(id, _)| id == key) {
            return Err(Error::invalid(
                "invalid_config",
                format!("unknown roleDefaults key '{key}'"),
            ));
        }
    }
    let pins_in = obj
        .get("pins")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::invalid("invalid_config", "config.pins must be an array"))?;
    let mut pins = Vec::new();
    let mut seen_pins = BTreeSet::new();
    for pin in pins_in {
        let pkg = str_field(pin, "pkg")?;
        let pinned = str_field(pin, "pinned")?;
        check_short("pin package", &pkg, 80)?;
        check_short("pin version", &pinned, 80)?;
        let spec = format!("{pkg}@{pinned}");
        parse_pin_text(&spec)?;
        if !seen_pins.insert(spec) {
            return Err(Error::invalid(
                "invalid_config",
                format!("duplicate pin '{pkg}'"),
            ));
        }
        pins.push(json!({"pkg": pkg, "pinned": pinned}));
    }
    Ok(Overlay {
        harnesses: norm_h,
        providers: norm_p,
        models: norm_m,
        scope,
        role_defaults,
        pins,
    })
}

fn parse_profiles(config: &Value, overlay: &Overlay) -> Result<Vec<Profile>> {
    let rows = config
        .get("profiles")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::invalid("invalid_config", "config.profiles must be an array"))?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for row in rows {
        let id = str_field(row, "id")?;
        if id == "master" {
            return Err(Error::rejected(
                "profile id 'master' is the master briefing, not a settings profile",
            ));
        }
        check_slug(&id)?;
        let role = str_field(row, "role")?;
        if !crate::model_defaults::canonical_team_role(&role) {
            return Err(Error::invalid(
                "invalid_config",
                format!("profile '{id}' role '{role}' is not a team role"),
            ));
        }
        let model = validate_model_id(&str_field(row, "model")?)?;
        if model != "inherit" {
            let known = overlay
                .models
                .iter()
                .any(|row| row["id"].as_str() == Some(model.as_str()));
            if !known {
                return Err(Error::invalid(
                    "invalid_config",
                    format!("profile '{id}' names unknown model '{model}'"),
                ));
            }
        }
        let job = str_field(row, "job")?;
        if job.trim().is_empty() {
            return Err(Error::invalid(
                "invalid_config",
                format!("profile '{id}' job is empty"),
            ));
        }
        check_markdown("profile job", &job, crate::master::AGENT_MAX_CHARS)?;
        if !seen.insert(id.clone()) {
            return Err(Error::invalid(
                "invalid_config",
                format!("duplicate profile '{id}'"),
            ));
        }
        let profile = Profile {
            id,
            role,
            model,
            job,
            text: String::new(),
        };
        out.push(profile);
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

fn check_slug(slug: &str) -> Result<()> {
    if slug.is_empty()
        || slug.len() > 64
        || !slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(Error::rejected(format!(
            "Bad agent slug '{slug}' — lowercase letters, digits and '-'"
        )));
    }
    Ok(())
}

fn ids_of(rows: &[Value]) -> BTreeSet<&str> {
    rows.iter().filter_map(|row| row["id"].as_str()).collect()
}

fn check_references(
    previous_overlay: &Overlay,
    overlay: &Overlay,
    profiles: &[Profile],
    previous: &[Profile],
    agents: &[Agent],
) -> Result<()> {
    let next_models = ids_of(&overlay.models);
    for profile in profiles {
        if profile.model != "inherit" && !next_models.contains(profile.model.as_str()) {
            return Err(Error::invalid(
                "dangling_reference",
                format!(
                    "profile '{}' still names model '{}'",
                    profile.id, profile.model
                ),
            ));
        }
    }
    let prev_models = ids_of(&previous_overlay.models);
    let removed_models: Vec<&str> = prev_models.difference(&next_models).copied().collect();
    let prev_harness = ids_of(&previous_overlay.harnesses);
    let next_harness = ids_of(&overlay.harnesses);
    let prev_providers = ids_of(&previous_overlay.providers);
    let next_providers = ids_of(&overlay.providers);
    for agent in agents.iter().filter(|a| active(a)) {
        if let Some(model) = agent_model(agent) {
            if removed_models.contains(&model) {
                return Err(Error::rejected(format!(
                    "model '{model}' is still used by active agent '{}'",
                    agent.alias
                )));
            }
        }
        if let Some(id) = agent_profile(agent) {
            if previous.iter().any(|p| p.id == id) && !profiles.iter().any(|p| p.id == id) {
                return Err(Error::rejected(format!(
                    "profile '{id}' is still used by active agent '{}'",
                    agent.alias
                )));
            }
        }
        if prev_harness.contains(agent.provider.as_str())
            && !next_harness.contains(agent.provider.as_str())
        {
            return Err(Error::rejected(format!(
                "harness '{}' still has active agent '{}'",
                agent.provider, agent.alias
            )));
        }
        if prev_providers.contains(agent.provider.as_str())
            && !next_providers.contains(agent.provider.as_str())
        {
            return Err(Error::rejected(format!(
                "provider '{}' still has active agent '{}'",
                agent.provider, agent.alias
            )));
        }
    }
    Ok(())
}

fn project_defaults(current: &ModelDefaults, overlay: &Overlay) -> Result<ModelDefaults> {
    let mut config = current.clone();
    let previous = overlay_from(&current.agent_settings);
    for (role, _) in TEAM_ROLES {
        let old = previous
            .role_defaults
            .get(*role)
            .map(String::as_str)
            .unwrap_or("");
        let new = overlay
            .role_defaults
            .get(*role)
            .map(String::as_str)
            .unwrap_or("");
        if old.is_empty() || old == new {
            continue;
        }
        let Some(target) = launch_target(&previous, old) else {
            continue;
        };
        let Some(provider) = config.providers.get_mut(&target) else {
            continue;
        };
        if provider.roles.get(*role).and_then(selector_model) == Some(old) {
            provider.roles.remove(*role);
        }
    }
    for (role, model) in &overlay.role_defaults {
        if model.is_empty() {
            continue;
        }
        let target = launch_target(overlay, model).ok_or_else(|| {
            Error::invalid(
                "invalid_config",
                format!("role default '{model}' has no launch provider"),
            )
        })?;
        config
            .providers
            .entry(target)
            .or_insert_with(|| ProviderDefaults {
                default: ModelSelector::ProviderDefault,
                roles: BTreeMap::new(),
            })
            .roles
            .insert(
                role.clone(),
                ModelSelector::Model {
                    model: model.clone(),
                },
            );
    }
    config.agent_settings = overlay_value(overlay);
    Ok(config)
}

fn launch_target(overlay: &Overlay, model: &str) -> Option<String> {
    let record = overlay
        .models
        .iter()
        .find(|row| row["id"].as_str() == Some(model))?;
    let provider = record["provider"].as_str().unwrap_or("");
    match provider_kind(overlay, provider) {
        "Pi extension" => Some("pi".to_string()),
        "native CLI" if registry::model_provider_ids().contains(&provider) => {
            Some(provider.to_string())
        }
        _ => None,
    }
}

fn selector_model(selector: &ModelSelector) -> Option<&str> {
    match selector {
        ModelSelector::Model { model } => Some(model.as_str()),
        ModelSelector::ProviderDefault => None,
    }
}

/// Role defaults shown to the UI follow the central provider map. A stored
/// overlay entry is kept only while that provider role is still the same
/// model. A reset or a native replacement becomes empty instead of a stale
/// claim. Unrelated provider roles are not copied into the map.
fn viewed_overlay(defaults: &ModelDefaults) -> Overlay {
    let mut overlay = overlay_from(&defaults.agent_settings);
    let mut roles = BTreeMap::new();
    for (role, _) in TEAM_ROLES {
        let claimed = overlay
            .role_defaults
            .get(*role)
            .cloned()
            .unwrap_or_default();
        let visible = if claimed.is_empty() {
            String::new()
        } else if launch_target(&overlay, &claimed)
            .as_deref()
            .and_then(|target| defaults.providers.get(target))
            .and_then(|provider| provider.roles.get(*role))
            .and_then(selector_model)
            == Some(claimed.as_str())
        {
            claimed
        } else {
            String::new()
        };
        roles.insert((*role).to_string(), visible);
    }
    overlay.role_defaults = roles;
    overlay
}

/// Show configured Pi ids that are not yet copied into the overlay. This
/// does not add catalog text. A later save stores whatever the client sends.
fn present_overlay(defaults: &ModelDefaults, policy: Option<&PiPolicy>) -> Overlay {
    let mut overlay = viewed_overlay(defaults);
    let Some(policy) = policy else {
        return overlay;
    };
    let mut known = BTreeSet::new();
    for model in &overlay.models {
        if let Some(id) = model["id"].as_str() {
            known.insert(id.to_string());
        }
    }
    let mut added = false;
    for (id, mode) in policy_scope_view(policy) {
        overlay.scope.entry(id.clone()).or_insert(mode);
        if known.insert(id.clone()) {
            overlay.models.push(json!({
                "id": id,
                "provider": "pi",
                "ctx": "",
                "out": "",
                "thinking": null,
                "images": null,
            }));
            added = true;
        }
    }
    if added
        && !overlay
            .providers
            .iter()
            .any(|row| row["kind"].as_str() == Some("Pi extension"))
    {
        overlay.providers.push(json!({
            "id": "pi",
            "label": "Pi",
            "kind": "Pi extension",
            "pin": "",
            "gate": true,
            "paid": false,
            "note": "",
        }));
    }
    for spec in &policy.providers {
        let Some((pkg, pinned)) = spec.rsplit_once('@') else {
            continue;
        };
        if overlay
            .pins
            .iter()
            .any(|row| row["pkg"].as_str() == Some(pkg))
        {
            continue;
        }
        overlay.pins.push(json!({"pkg": pkg, "pinned": pinned}));
    }
    if let Some(model) = policy.models.default.worker.as_deref() {
        let slot = overlay
            .role_defaults
            .entry("worker".to_string())
            .or_default();
        if slot.is_empty()
            && overlay
                .scope
                .get(model)
                .is_some_and(|mode| mode == "allow" || mode == "worker")
        {
            *slot = model.to_string();
        }
    }
    overlay
}

fn policy_scope_view(policy: &PiPolicy) -> Vec<(String, String)> {
    let mut ids: Vec<&str> = policy
        .models
        .allow
        .iter()
        .chain(policy.models.master_allow.iter().flatten())
        .chain(policy.models.worker_allow.iter().flatten())
        .chain(policy.models.default.master.iter())
        .chain(policy.models.default.worker.iter())
        .map(String::as_str)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids.into_iter()
        .map(|id| {
            let master = policy
                .models
                .allow_for("master")
                .iter()
                .any(|row| row == id);
            let worker = policy
                .models
                .allow_for("worker")
                .iter()
                .any(|row| row == id);
            let mode = match (master, worker) {
                (true, true) => "allow",
                (true, false) => "master",
                (false, true) => "worker",
                (false, false) => "off",
            };
            (id.to_string(), mode.to_string())
        })
        .collect()
}

fn overlay_value(overlay: &Overlay) -> Value {
    json!({
        "harnesses": overlay.harnesses,
        "providers": overlay.providers,
        "models": overlay.models,
        "scope": overlay.scope,
        "roleDefaults": overlay.role_defaults,
        "pins": overlay.pins,
    })
}

fn provider_kind<'a>(overlay: &'a Overlay, id: &str) -> &'a str {
    overlay
        .providers
        .iter()
        .find(|p| p["id"].as_str() == Some(id))
        .and_then(|p| p["kind"].as_str())
        .unwrap_or("")
}

fn profiles_from(bodies: &[crate::store::ProfileBody]) -> Result<Vec<Profile>> {
    bodies
        .iter()
        .map(|body| parse_agent(&body.id, &body.text))
        .collect()
}

fn pi_scopes(overlay: &Overlay) -> BTreeMap<String, String> {
    let mut scopes = BTreeMap::new();
    for model in &overlay.models {
        let id = model["id"].as_str().unwrap_or("");
        let provider = model["provider"].as_str().unwrap_or("");
        if provider_kind(overlay, provider) != "Pi extension" {
            continue;
        }
        let mode = overlay.scope.get(id).map(String::as_str).unwrap_or("off");
        scopes.insert(id.to_string(), mode.to_string());
    }
    scopes
}

fn pi_pins(overlay: &Overlay) -> BTreeSet<String> {
    let mut pins = BTreeSet::new();
    for pin in &overlay.pins {
        let pkg = pin["pkg"].as_str().unwrap_or("");
        let pinned = pin["pinned"].as_str().unwrap_or("");
        if !pkg.is_empty() && !pinned.is_empty() {
            pins.insert(format!("{pkg}@{pinned}"));
        }
    }
    for provider in &overlay.providers {
        if provider["kind"].as_str() != Some("Pi extension") {
            continue;
        }
        let pin = provider["pin"].as_str().unwrap_or("");
        if pin.is_empty() {
            continue;
        }
        if let Ok(spec) = normalize_pin(pin) {
            pins.insert(spec);
        }
    }
    pins
}

fn project_pi(stored: &Option<PiPolicy>, overlay: &Overlay) -> Result<Option<PiPolicy>> {
    let scopes = pi_scopes(overlay);
    let pins = pi_pins(overlay);
    if let Some(policy) = stored {
        let canonical_scopes: BTreeMap<String, String> =
            policy_scope_view(policy).into_iter().collect();
        let canonical_pins: BTreeSet<String> = policy.providers.iter().cloned().collect();
        if scopes == canonical_scopes && pins == canonical_pins {
            let mut kept = policy.clone();
            if let Some(model) = overlay.role_defaults.get("worker") {
                let visible = |id: &str| {
                    scopes
                        .get(id)
                        .is_some_and(|mode| mode == "allow" || mode == "worker")
                };
                if model.is_empty() {
                    if kept.models.default.worker.as_deref().is_some_and(visible) {
                        kept.models.default.worker = None;
                    }
                } else if visible(model) {
                    kept.models.default.worker = Some(model.clone());
                }
            }
            kept.validate_public()?;
            return Ok(Some(kept));
        }
    } else if scopes.is_empty() && pins.is_empty() {
        return Ok(None);
    }
    let mut policy = stored.clone().unwrap_or_default();
    let mut allow_scoped = Vec::new();
    let mut global_only = Vec::new();
    let mut master_only = Vec::new();
    let mut worker_only = Vec::new();
    let mut split = false;
    for model in &overlay.models {
        let id = model["id"].as_str().unwrap_or("");
        let provider = model["provider"].as_str().unwrap_or("");
        if provider_kind(overlay, provider) != "Pi extension" || id.is_empty() {
            continue;
        }
        match overlay.scope.get(id).map(String::as_str) {
            Some("allow") => allow_scoped.push(id.to_string()),
            Some("off") => {
                split = true;
                global_only.push(id.to_string());
            }
            Some("master") => {
                split = true;
                master_only.push(id.to_string());
            }
            Some("worker") => {
                split = true;
                worker_only.push(id.to_string());
            }
            _ => {
                split = true;
                global_only.push(id.to_string());
            }
        }
    }
    let mut pin_specs = Vec::new();
    for pin in &overlay.pins {
        let pkg = pin["pkg"].as_str().unwrap_or("");
        let pinned = pin["pinned"].as_str().unwrap_or("");
        if pkg.is_empty() || pinned.is_empty() {
            continue;
        }
        pin_specs.push(format!("{pkg}@{pinned}"));
    }
    for provider in &overlay.providers {
        if provider["kind"].as_str() != Some("Pi extension") {
            continue;
        }
        let pin = provider["pin"].as_str().unwrap_or("");
        if pin.is_empty() {
            continue;
        }
        let spec = normalize_pin(pin)?;
        if !pin_specs.iter().any(|e| e == &spec) {
            pin_specs.push(spec);
        }
    }
    pin_specs.sort();
    pin_specs.dedup();
    allow_scoped.sort();
    global_only.sort();
    master_only.sort();
    worker_only.sort();
    let mut allow = allow_scoped.clone();
    allow.extend(global_only.iter().cloned());
    allow.extend(master_only.iter().cloned());
    allow.extend(worker_only.iter().cloned());
    allow.sort();
    allow.dedup();
    let (master_allow, worker_allow) = if split {
        let mut master_list = allow_scoped.clone();
        master_list.extend(master_only.iter().cloned());
        master_list.sort();
        let mut worker_list = allow_scoped.clone();
        worker_list.extend(worker_only.iter().cloned());
        worker_list.sort();
        (Some(master_list), Some(worker_list))
    } else {
        (None, None)
    };
    policy.providers = pin_specs.clone();
    policy.models.allow = allow.clone();
    policy.models.master_allow = master_allow;
    policy.models.worker_allow = worker_allow;
    let launchable = |model: &str| allow.iter().any(|item| item == model);
    if policy
        .models
        .default
        .master
        .as_ref()
        .is_some_and(|model| !launchable(model))
    {
        policy.models.default.master = None;
    }
    if policy
        .models
        .default
        .worker
        .as_ref()
        .is_some_and(|model| !launchable(model))
    {
        policy.models.default.worker = None;
    }
    if allow.is_empty() && pin_specs.is_empty() {
        policy.models.allow.clear();
        policy.models.master_allow = None;
        policy.models.worker_allow = None;
        policy.models.default.master = None;
        policy.models.default.worker = None;
        policy.providers.clear();
    } else if let Some(model) = overlay.role_defaults.get("worker") {
        policy.models.default.worker = if !model.is_empty()
            && policy
                .models
                .allow_for("worker")
                .iter()
                .any(|item| item == model)
        {
            Some(model.clone())
        } else {
            None
        };
    }
    policy.validate_public()?;
    Ok(Some(policy))
}

fn build_snapshot(
    token: String,
    overlay: &Overlay,
    profiles: &[Profile],
    policy: Option<&PiPolicy>,
    agents: &[Agent],
    pm_dir: &Path,
    history: &[Value],
) -> Value {
    let mut harness_obs = Map::new();
    for harness in &overlay.harnesses {
        let id = harness["id"].as_str().unwrap_or("");
        let cli = harness["cli"].as_str().unwrap_or("");
        let (installed, version) = match registry_cli(id, cli) {
            Some(spec) => {
                let (ok, version) = probe_cli(spec, cli);
                (Some(ok), version)
            }
            None => (None, None),
        };
        let live = agents
            .iter()
            .filter(|a| active(a) && a.provider == id)
            .count();
        harness_obs.insert(
            id.to_string(),
            json!({"installed": installed, "version": version, "live": live}),
        );
    }
    let mut model_obs = Map::new();
    for model in &overlay.models {
        let id = model["id"].as_str().unwrap_or("");
        let live = agents
            .iter()
            .filter(|a| active(a) && agent_model(a) == Some(id))
            .count();
        model_obs.insert(id.to_string(), json!({"live": live}));
    }
    let mut profile_obs = Map::new();
    let mut profile_rows = Vec::new();
    for profile in profiles {
        let files: Vec<Value> = ["pi", "claude"]
            .into_iter()
            .map(|harness| {
                let path = export_path(pm_dir, harness, &profile.id);
                json!({
                    "harness": harness,
                    "path": path.display().to_string(),
                    "status": export_status(pm_dir, harness, &profile.id, profile),
                })
            })
            .collect();
        let used: Vec<&str> = agents
            .iter()
            .filter(|a| agent_profile(a) == Some(profile.id.as_str()))
            .map(|a| a.alias.as_str())
            .collect();
        profile_obs.insert(
            profile.id.clone(),
            json!({
                "version": digest(&profile.text)[..12],
                "usedBy": used,
                "history": profile_history(history, &profile.id),
                "files": files,
            }),
        );
        profile_rows.push(json!({
            "id": profile.id,
            "role": profile.role,
            "model": profile.model,
            "job": profile.job,
        }));
    }
    let mut pins = Vec::new();
    if let Some(policy) = policy {
        for spec in &policy.providers {
            if let Some((pkg, pinned)) = spec.rsplit_once('@') {
                pins.push(json!({"pkg": pkg, "pinned": pinned}));
            }
        }
    }
    if pins.is_empty() {
        pins = overlay.pins.clone();
    }
    json!({
        "revision": token,
        "config": {
            "harnesses": overlay.harnesses,
            "providers": overlay.providers,
            "models": overlay.models,
            "profiles": profile_rows,
            "scope": overlay.scope,
            "roleDefaults": overlay.role_defaults,
            "pins": pins,
        },
        "observations": {
            "harnesses": harness_obs,
            "models": model_obs,
            "profiles": profile_obs,
        },
        "catalogs": {},
        "history": all_profile_changes(history),
    })
}

fn history_row(event: &Value, change: &Value) -> Value {
    let revision = event.get("revision").and_then(Value::as_i64);
    json!({
        "id": change.get("id").and_then(Value::as_str).unwrap_or(""),
        "at": revision.map(|n| format!("rev {n}")).unwrap_or_else(|| "rev".to_string()),
        "what": change.get("what").and_then(Value::as_str).unwrap_or("updated"),
        "revision": revision,
        "actor": event.get("actor").cloned().unwrap_or(Value::Null),
        "tombstone": change.get("tombstone").cloned().unwrap_or(json!(false)),
    })
}

fn profile_history(history: &[Value], id: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for event in history {
        let Some(changes) = event.get("changes").and_then(Value::as_array) else {
            continue;
        };
        for change in changes {
            if change.get("id").and_then(Value::as_str) != Some(id) {
                continue;
            }
            out.push(history_row(event, change));
        }
    }
    out
}

fn all_profile_changes(history: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for event in history {
        let Some(changes) = event.get("changes").and_then(Value::as_array) else {
            continue;
        };
        for change in changes {
            out.push(history_row(event, change));
        }
    }
    out
}

/// After import, `pm.yaml` `[pi]` is not a launch input. A byte difference
/// from the settings row is drift; a match is only a retired source.
fn retired_pi(pm_dir: &Path, imported: bool, stored: &Option<PiPolicy>) -> Vec<Value> {
    if !imported {
        return Vec::new();
    }
    let status = match pi_policy::read(pm_dir) {
        Err(_) => "drift",
        Ok(on_disk) => {
            let same = serde_json::to_value(&on_disk).ok() == serde_json::to_value(stored).ok();
            if same {
                "retired"
            } else {
                "drift"
            }
        }
    };
    vec![json!({
        "source": pm_dir.join("pm.yaml").display().to_string(),
        "section": "[pi]",
        "status": status,
        "effect": "launch uses the settings row, not this file",
    })]
}

fn with_retired(mut snapshot: Value, sources: Vec<Value>) -> Value {
    snapshot["retiredSources"] = json!(sources);
    snapshot
}

fn parse_agent(slug: &str, text: &str) -> Result<Profile> {
    let Some(rest) = text.strip_prefix("---\n") else {
        return Ok(Profile {
            id: slug.to_string(),
            role: "worker".to_string(),
            model: "inherit".to_string(),
            job: text.trim().to_string(),
            text: text.to_string(),
        });
    };
    let Some((front, body)) = rest.split_once("\n---\n") else {
        return Ok(Profile {
            id: slug.to_string(),
            role: "worker".to_string(),
            model: "inherit".to_string(),
            job: text.trim().to_string(),
            text: text.to_string(),
        });
    };
    let mut role = None;
    let mut model = None;
    for line in front.lines() {
        let Some((key, raw)) = line.split_once(':') else {
            continue;
        };
        if !matches!(key.trim(), "role" | "model") {
            continue;
        }
        let raw = raw.trim();
        let value = if raw.starts_with('"') || raw.starts_with('\'') {
            serde_yaml::from_str::<String>(raw).map_err(|err| {
                Error::invalid(
                    "invalid_config",
                    format!("profile '{slug}' {}: {err}", key.trim()),
                )
            })?
        } else {
            raw.to_string()
        };
        match key.trim() {
            "role" => role = Some(value),
            "model" => model = Some(value),
            _ => unreachable!(),
        }
    }
    let role = role.unwrap_or_default();
    let role = if role.is_empty() {
        "worker".to_string()
    } else {
        role
    };
    let model = model.unwrap_or_else(|| "inherit".to_string());
    Ok(Profile {
        id: slug.to_string(),
        role,
        model,
        job: body.trim().to_string(),
        text: text.to_string(),
    })
}

fn render_agent(profile: &Profile) -> String {
    format!(
        "---\n{PROFILE_MARKER}: \"1\"\nrole: {}\nmodel: {}\n---\n{}\n",
        serde_json::to_string(&profile.role).expect("profile role is a serializable string"),
        serde_json::to_string(&profile.model).expect("profile model is a serializable string"),
        profile.job.trim()
    )
}

fn render_export(profile: &Profile) -> String {
    let stamp = &digest(&profile.text)[..12];
    format!(
        "---\n# {GEN_MARKER} {stamp}\nname: {}\ndescription: {}\nmodel: {}\n---\n{}\n",
        serde_json::to_string(&profile.id).expect("profile id is a serializable string"),
        serde_json::to_string(profile.job.lines().next().unwrap_or_default().trim())
            .expect("profile description is a serializable string"),
        serde_json::to_string(&profile.model).expect("profile model is a serializable string"),
        profile.job.trim()
    )
}

fn export_path(pm_dir: &Path, harness: &str, id: &str) -> PathBuf {
    let dir = match harness {
        "claude" => ".claude/agents",
        _ => ".pi/agents",
    };
    pm_dir.join(dir).join(format!("{id}.md"))
}

fn export_status(pm_dir: &Path, harness: &str, id: &str, profile: &Profile) -> &'static str {
    match read_export(pm_dir, harness, id) {
        ExportRead::Missing => "not written",
        ExportRead::Keep => "unmanaged",
        ExportRead::Body(text) if text == render_export(profile) => "in sync",
        ExportRead::Body(_) => "needs sync",
    }
}

fn write_export(pm_dir: &Path, harness: &str, id: &str, profile: &Profile) -> Result<()> {
    if !safe_export_id(id) {
        return Err(Error::rejected(
            "profile id cannot be used as an export file name",
        ));
    }
    let Some(root) = open_export_root(pm_dir)? else {
        return Err(Error::rejected("PM directory is missing"));
    };
    let Some(dir) = walk_export_dir(&root, harness, true)? else {
        return Err(Error::rejected("export directory is missing"));
    };
    let name = format!("{id}.md");
    write_export_at(&dir, &name, &render_export(profile))
}

fn resolve_model(profile: &Profile, overlay: &Overlay) -> Option<(String, String)> {
    if profile.model != "inherit" {
        return Some((profile.model.clone(), "profile".to_string()));
    }
    let default = overlay.role_defaults.get(&profile.role)?;
    if default.is_empty() {
        return None;
    }
    Some((default.clone(), "role_default".to_string()))
}

pub(crate) fn validate_model_harness(
    model: &str,
    harness: &str,
    models: &[Value],
    providers: &[Value],
) -> Result<()> {
    let Some(record) = models
        .iter()
        .find(|record| record["id"].as_str() == Some(model))
    else {
        return Ok(());
    };
    let owner = record["provider"].as_str().unwrap_or("");
    let kind = providers
        .iter()
        .find(|provider| provider["id"].as_str() == Some(owner))
        .and_then(|provider| provider["kind"].as_str());
    let expected = match kind {
        Some("Pi extension") => "pi",
        Some("native CLI") => owner,
        _ => {
            return Err(Error::rejected(format!(
                "model '{model}' has no registered provider"
            )))
        }
    };
    if expected != harness {
        return Err(Error::rejected(format!(
            "model '{model}' belongs to harness '{expected}', not '{harness}'"
        )));
    }
    Ok(())
}

fn permission_reasons(
    model: &str,
    overlay: &Overlay,
    spec: Option<&EndpointSpec>,
    policy: Option<&PiPolicy>,
    role: &str,
) -> Vec<String> {
    let mut reasons = Vec::new();
    let record = overlay
        .models
        .iter()
        .find(|m| m["id"].as_str() == Some(model));
    let provider = record.and_then(|m| m["provider"].as_str()).unwrap_or("");
    let kind = provider_kind(overlay, provider);
    let scope = overlay.scope.get(model).map(String::as_str).unwrap_or("");
    let harness_provider = spec.map(|s| s.provider).unwrap_or("");
    if kind == "Pi extension" || harness_provider == "pi" {
        if scope == "off" || scope.is_empty() {
            reasons.push(format!("model '{model}' scope is off"));
        }
        if scope == "master" {
            reasons.push(format!(
                "model '{model}' is scoped to the master runtime; profile role '{role}' does not grant it"
            ));
        }
    }
    if let Err(err) =
        validate_model_harness(model, harness_provider, &overlay.models, &overlay.providers)
    {
        reasons.push(err.to_string());
    }
    if kind == "Pi extension" || harness_provider == "pi" {
        if let Err(err) = pi_policy::require_allowed(policy, "worker", model) {
            reasons.push(err.to_string());
        }
    }
    reasons
}

fn registry_cli(harness: &str, cli: &str) -> Option<&'static EndpointSpec> {
    registry::SPECS.iter().find(|spec| {
        !spec.internal
            && spec.provider == harness
            && spec.probe_bins.iter().any(|(program, _)| *program == cli)
    })
}

fn probe_cli(spec: &EndpointSpec, cli: &str) -> (bool, Option<String>) {
    let Some((program, args)) = spec.probe_bins.iter().find(|(program, _)| *program == cli) else {
        return (false, None);
    };
    let mut cmd = Command::new(program);
    cmd.args(*args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = match crate::reaper::spawn(&mut cmd) {
        Ok(child) => child,
        Err(_) => return (false, None),
    };
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if start.elapsed() < Duration::from_secs(2) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return (true, None);
            }
        }
    };
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout {
        let _ = stdout.read_to_string(&mut out);
    }
    let version = status
        .filter(|s| s.success())
        .and_then(|_| out.lines().next().map(str::trim))
        .filter(|line| !line.is_empty())
        .map(str::to_string);
    (true, version)
}

fn active(agent: &Agent) -> bool {
    !matches!(agent.state.as_str(), "stopped" | "offline")
}

fn agent_model(agent: &Agent) -> Option<&str> {
    agent
        .params
        .as_ref()
        .and_then(|p| p.get("model"))
        .and_then(Value::as_str)
        .or(agent.model.as_deref())
}

fn agent_profile(agent: &Agent) -> Option<&str> {
    agent
        .params
        .as_ref()
        .and_then(|p| p.get("profile"))
        .and_then(Value::as_str)
}

fn digest(text: &str) -> String {
    let hash = Sha256::digest(text.as_bytes());
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

enum ExportRead {
    Missing,
    Keep,
    Body(String),
}

fn safe_export_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains('\0')
}

fn export_component(name: &str) -> Result<CString> {
    CString::new(name).map_err(|_| Error::rejected("export path contains an interior nul"))
}

fn open_export_root(pm_dir: &Path) -> Result<Option<OwnedFd>> {
    let name = CString::new(pm_dir.as_os_str().as_bytes())
        .map_err(|_| Error::rejected("PM directory contains an interior nul"))?;
    let fd = unsafe {
        libc::open(
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(Error::from(err));
    }
    Ok(Some(unsafe { OwnedFd::from_raw_fd(fd) }))
}

fn kind_at(dir: &OwnedFd, name: &str) -> Result<AtExport> {
    let c_name = export_component(name)?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            c_name.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::NotFound {
            return Ok(AtExport::Missing);
        }
        return Err(Error::from(err));
    }
    Ok(match st.st_mode & libc::S_IFMT {
        mode if mode == libc::S_IFLNK => AtExport::Symlink,
        mode if mode == libc::S_IFDIR => AtExport::Dir,
        _ => AtExport::Other,
    })
}

enum AtExport {
    Missing,
    Symlink,
    Dir,
    Other,
}

fn open_export_dir(dir: &OwnedFd, name: &str) -> Result<OwnedFd> {
    let c_name = export_component(name)?;
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            c_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(Error::rejected(format!(
            "{name} is a symlink or not a directory"
        )));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn mkdir_export(dir: &OwnedFd, name: &str) -> Result<()> {
    let c_name = export_component(name)?;
    let rc = unsafe { libc::mkdirat(dir.as_raw_fd(), c_name.as_ptr(), 0o755) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.kind() == std::io::ErrorKind::AlreadyExists {
        return Ok(());
    }
    Err(Error::from(err))
}

fn open_or_make(parent: &OwnedFd, name: &str, create: bool) -> Result<Option<OwnedFd>> {
    match kind_at(parent, name)? {
        AtExport::Missing if !create => Ok(None),
        AtExport::Missing => {
            mkdir_export(parent, name)?;
            Ok(Some(open_export_dir(parent, name)?))
        }
        AtExport::Dir => Ok(Some(open_export_dir(parent, name)?)),
        AtExport::Symlink | AtExport::Other => Err(Error::rejected(format!(
            "{name} is a symlink or not a directory; refusing to follow it"
        ))),
    }
}

fn walk_export_dir(root: &OwnedFd, harness: &str, create: bool) -> Result<Option<OwnedFd>> {
    let top = if harness == "claude" {
        ".claude"
    } else {
        ".pi"
    };
    let Some(first) = open_or_make(root, top, create)? else {
        return Ok(None);
    };
    open_or_make(&first, "agents", create)
}

fn read_regular_at(dir: &OwnedFd, name: &str) -> Result<Option<String>> {
    let c_name = export_component(name)?;
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            c_name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(Error::rejected(format!(
            "{name} is a symlink or cannot be opened"
        )));
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    let meta = file.metadata()?;
    if !meta.file_type().is_file() {
        return Err(Error::rejected(format!("{name} is not a regular file")));
    }
    let mut text = String::new();
    Read::take(&mut file, 1024 * 1024).read_to_string(&mut text)?;
    Ok(Some(text))
}

fn read_export(pm_dir: &Path, harness: &str, id: &str) -> ExportRead {
    if !safe_export_id(id) {
        return ExportRead::Keep;
    }
    let Ok(Some(root)) = open_export_root(pm_dir) else {
        return ExportRead::Missing;
    };
    let dir = match walk_export_dir(&root, harness, false) {
        Ok(Some(dir)) => dir,
        Ok(None) => return ExportRead::Missing,
        Err(_) => return ExportRead::Keep,
    };
    match read_regular_at(&dir, &format!("{id}.md")) {
        Ok(None) => ExportRead::Missing,
        Ok(Some(text)) if text.starts_with(&format!("---\n# {GEN_MARKER} ")) => {
            ExportRead::Body(text)
        }
        Ok(Some(_)) | Err(_) => ExportRead::Keep,
    }
}

fn write_export_at(dir: &OwnedFd, name: &str, text: &str) -> Result<()> {
    let cname = export_component(name)?;
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            cname.as_ptr(),
            libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd >= 0 {
        let mut file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(Error::rejected("export is not a private regular file"));
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(Error::rejected("export is busy; retry sync"));
        }
        let mut previous = String::new();
        Read::take(&mut file, 1024 * 1024).read_to_string(&mut previous)?;
        if !previous.starts_with(&format!("---\n# {GEN_MARKER} ")) {
            return Err(Error::rejected(format!(
                "{name} is unmanaged; refusing to write it"
            )));
        }
        // Write the validated inode, never a replacement at `name`. Preserve
        // the ownership header through partial writes; truncate only afterward.
        file.seek(SeekFrom::Start(0))?;
        file.write_all(text.as_bytes())?;
        file.set_len(text.len() as u64)?;
        file.sync_all()?;
        let current_fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                cname.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if current_fd < 0 {
            return Err(Error::rejected("export destination changed during sync"));
        }
        let current = unsafe { File::from_raw_fd(current_fd) }.metadata()?;
        if current.dev() != metadata.dev() || current.ino() != metadata.ino() {
            return Err(Error::rejected("export destination changed during sync"));
        }
        return Ok(());
    }
    if std::io::Error::last_os_error().kind() != std::io::ErrorKind::NotFound {
        return Err(Error::rejected("export is a symlink or cannot be opened"));
    }
    let tmp = format!(
        ".{}.{}.tmp",
        name.trim_end_matches(".md"),
        uuid::Uuid::new_v4().simple()
    );
    let ctmp = export_component(&tmp)?;
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            ctmp.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o644,
        )
    };
    if fd < 0 {
        return Err(Error::rejected(
            "export temp could not be created exclusively",
        ));
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    let written = file
        .write_all(text.as_bytes())
        .and_then(|_| file.sync_all());
    drop(file);
    if written.is_err() {
        unsafe { libc::unlinkat(dir.as_raw_fd(), ctmp.as_ptr(), 0) };
        return Err(Error::rejected("export write failed"));
    }
    // Publish only if the destination is still absent; never replace an entry
    // another writer created after the absence check.
    let rc = unsafe {
        libc::linkat(
            dir.as_raw_fd(),
            ctmp.as_ptr(),
            dir.as_raw_fd(),
            cname.as_ptr(),
            0,
        )
    };
    unsafe { libc::unlinkat(dir.as_raw_fd(), ctmp.as_ptr(), 0) };
    if rc != 0 {
        return Err(Error::rejected("export destination appeared during sync"));
    }
    if unsafe { libc::fsync(dir.as_raw_fd()) } != 0 {
        return Err(Error::rejected("export directory sync failed"));
    }
    Ok(())
}

fn iso_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    chrono::DateTime::from_timestamp(secs as i64, 0)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

/// Format a token count the listing actually returned. Missing stays blank.
fn token_field(row: &Value, key: &str) -> String {
    let Some(n) = row.get(key).and_then(Value::as_u64) else {
        return String::new();
    };
    if n >= 1_000_000 {
        let whole = n / 1_000_000;
        let tenth = (n % 1_000_000) / 100_000;
        if tenth == 0 {
            format!("{whole}M")
        } else {
            format!("{whole}.{tenth}M")
        }
    } else if n >= 1_000 {
        let whole = n / 1_000;
        let tenth = (n % 1_000) / 100;
        if tenth == 0 {
            format!("{whole}K")
        } else {
            format!("{whole}.{tenth}K")
        }
    } else {
        n.to_string()
    }
}

/// `input` containing `image` is the observed image capability. A missing
/// `input` stays unknown; a present list without `image` is false.
fn image_input(row: &Value) -> Value {
    match row.get("input").and_then(Value::as_array) {
        None => Value::Null,
        Some(kinds) => json!(kinds.iter().any(|k| k.as_str() == Some("image"))),
    }
}

fn array_field<'a>(obj: &'a Map<String, Value>, key: &str) -> Result<&'a Vec<Value>> {
    obj.get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| Error::invalid("invalid_config", format!("config.{key} must be an array")))
}

fn str_field(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| Error::invalid("invalid_config", format!("'{key}' must be a string")))
}

fn bool_req(value: &Value, key: &str) -> Result<bool> {
    value
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| Error::invalid("invalid_config", format!("'{key}' must be a boolean")))
}

fn tri_bool(value: &Value, key: &str) -> Result<Value> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(Value::Null),
        Some(Value::Bool(flag)) => Ok(json!(flag)),
        _ => Err(Error::invalid(
            "invalid_config",
            format!("'{key}' must be a boolean or null"),
        )),
    }
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

pub struct LaunchProfile {
    pub id: String,
    pub role: String,
    pub model: String,
    pub job: String,
    pub revision: i64,
    pub digest: String,
}

/// Resolve a settings profile for registration. The job is the instruction
/// text. `role` is the team-role lookup, not the runtime role.
pub fn launch_profile(
    store: &Store,
    id: &str,
    provider: &str,
    endpoint: &str,
    requested_model: Option<&str>,
    provider_default: bool,
) -> Result<LaunchProfile> {
    let current = store.settings()?;
    let body = current
        .doc
        .profiles
        .iter()
        .find(|profile| profile.id == id)
        .ok_or_else(|| Error::rejected(format!("unknown profile '{id}'")))?;
    let profile = parse_agent(&body.id, &body.text)?;
    if profile.job.len() > crate::store::MAX_INSTRUCTIONS_BYTES {
        return Err(Error::rejected(format!(
            "profile '{id}' job exceeds the {} UTF-8 byte launch limit",
            crate::store::MAX_INSTRUCTIONS_BYTES,
        )));
    }
    let requested_model = requested_model.map(validate_model_id).transpose()?;
    let overlay = present_overlay(&current.doc.model_defaults, current.doc.pi.as_ref());
    let model = resolve_model(&profile, &overlay)
        .map(|(id, _)| id)
        .unwrap_or_else(|| "inherit".to_string());
    let selected = requested_model
        .as_deref()
        .or_else(|| (!provider_default && model != "inherit").then_some(model.as_str()));
    if let Some(selected) = selected {
        if !registry::supports_model(provider, endpoint) {
            return Err(Error::rejected(format!(
                "harness '{provider}' does not accept a model"
            )));
        }
        let record = overlay
            .models
            .iter()
            .find(|record| record["id"].as_str() == Some(selected));
        if record.is_none() && requested_model.is_none() {
            return Err(Error::rejected(format!(
                "profile model '{selected}' has no registered provider"
            )));
        }
        validate_model_harness(selected, provider, &overlay.models, &overlay.providers)?;
    }
    Ok(LaunchProfile {
        id: profile.id,
        role: profile.role,
        model,
        job: profile.job,
        revision: current.revision,
        digest: digest(&body.text),
    })
}

fn check_markdown(label: &str, text: &str, cap: usize) -> Result<()> {
    if text.chars().count() > cap
        || text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(Error::invalid(
            "invalid_config",
            format!("{label} must be at most {cap} characters; newlines and tabs are allowed"),
        ));
    }
    Ok(())
}

fn check_short(label: &str, text: &str, cap: usize) -> Result<()> {
    if text.chars().count() > cap || text.chars().any(char::is_control) {
        return Err(Error::invalid(
            "invalid_config",
            format!("{label} must be at most {cap} characters without control characters"),
        ));
    }
    Ok(())
}

fn check_ident(label: &str, text: &str) -> Result<()> {
    if text.is_empty()
        || text.len() > 64
        || !text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(Error::invalid(
            "invalid_config",
            format!("{label} '{text}' must be 1-64 letters, digits, '.', '_' or '-'"),
        ));
    }
    Ok(())
}

fn parse_pin_text(spec: &str) -> Result<()> {
    let Some((name, version)) = spec.rsplit_once('@') else {
        return Err(Error::invalid(
            "invalid_config",
            format!("pin '{spec}' must be name@version"),
        ));
    };
    if name.is_empty()
        || version.is_empty()
        || name.chars().any(char::is_control)
        || version.chars().any(|c| c.is_control() || c == '@')
    {
        return Err(Error::invalid(
            "invalid_config",
            format!("pin '{spec}' must be name@version"),
        ));
    }
    Ok(())
}

fn normalize_pin(pin: &str) -> Result<String> {
    if let Some((name, version)) = pin.rsplit_once('@') {
        let spec = format!("{name}@{version}");
        parse_pin_text(&spec)?;
        return Ok(spec);
    }
    if let Some((name, version)) = pin.split_once(' ') {
        if !version.contains(' ') {
            let spec = format!("{name}@{version}");
            parse_pin_text(&spec)?;
            return Ok(spec);
        }
    }
    Err(Error::invalid(
        "invalid_config",
        format!("provider pin '{pin}' must be name@version or 'name version'"),
    ))
}
