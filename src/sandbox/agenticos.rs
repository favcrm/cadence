//! Real AgenticOS provider profile setup, app binding, and sandbox approval flows.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::Path;

use serde_json::{json, Value};

use crate::client;
use crate::error::{Error, Result};

use super::{
    dev_owner, profile, provider, read_marker, refuse_production, refuse_unmarked_nonempty,
    require_marker, AgenticosAction, Sandbox, DEFAULT_NAME,
};

pub(super) fn run_agenticos(_state_dir: &Path, action: &AgenticosAction) -> Result<Value> {
    match action {
        AgenticosAction::Setup {
            name,
            origin,
            workspace,
            pin,
            token_dir,
        } => {
            let sb = agenticos_sandbox(name.as_deref())?;
            refuse_production(&sb)?;
            let existing = read_marker(&sb)?;
            refuse_unmarked_nonempty(&sb, existing.as_ref())?;
            std::fs::create_dir_all(&sb.root)?;
            // `up` refuses a non-empty root without a marker. Setup may run
            // before `up`, so leave the minimal marker `up` would accept.
            if existing.is_none() {
                std::fs::write(
                    sb.marker(),
                    json!({
                        "name": sb.name,
                        "created_at": crate::issue::time::iso(crate::issue::time::now_epoch()),
                    })
                    .to_string(),
                )?;
            }
            provider::setup(&sb.root, origin, workspace, pin, token_dir)
        }
        AgenticosAction::Bind {
            name,
            bundle,
            context,
            destination_id,
            timezone,
            no_publish,
        } => bind_agenticos(
            name.as_deref(),
            bundle,
            context,
            destination_id.as_deref(),
            timezone,
            !*no_publish,
        ),
        AgenticosAction::Approve {
            name,
            install_id,
            context_id,
            dry_run,
        } => approve_agenticos(name.as_deref(), install_id, context_id.as_deref(), *dry_run),
    }
}

/// The sandbox a dev-agenticos verb acts on: the named one, else the
/// profile this shell points at, else `dev`.
fn agenticos_sandbox(name: Option<&str>) -> Result<Sandbox> {
    let name = name
        .map(str::to_owned)
        .or_else(profile)
        .unwrap_or_else(|| DEFAULT_NAME.to_owned());
    if let Some(profile) = profile() {
        if name != profile {
            return Err(Error::rejected(format!(
                "this shell points at sandbox '{profile}', not '{name}' — pass --name {name} \
                 from a shell without CADENCE_PROFILE"
            )));
        }
    }
    let sb = Sandbox::open(&name)?;
    refuse_production(&sb)?;
    Ok(sb)
}

fn bind_agenticos(
    name: Option<&str>,
    bundle: &Path,
    context: &str,
    destination_id: Option<&str>,
    timezone: &str,
    publish: bool,
) -> Result<Value> {
    let sb = agenticos_sandbox(name)?;
    require_marker(&sb)?;
    let profile = provider::load(&sb.root)?
        .ok_or_else(|| Error::rejected("run `cadence dev agenticos setup` first"))?;
    let bundle = std::fs::canonicalize(bundle)
        .map_err(|_| Error::rejected("app bundle path does not exist"))?;
    if !bundle.join("app.md").is_file() {
        return Err(Error::rejected("app bundle has no app.md at its root"));
    }
    let manifest = crate::issue::app::parse_manifest(
        &std::fs::read_to_string(bundle.join("app.md"))
            .map_err(|_| Error::rejected("app.md is unreadable"))?,
    )?;

    let installed = match client::rpc_state(
        &sb.state_dir(),
        "app_workspace_install",
        json!({"source": bundle}),
    ) {
        Ok(installed) => installed,
        Err(error)
            if error
                .to_string()
                .contains("already has workspace installation") =>
        {
            return Err(Error::rejected(format!(
                "{error}; reset the sandbox (`cadence dev reset {}`) or upgrade the \
                 existing installation before rebinding",
                sb.name
            )))
        }
        Err(error) => return Err(error),
    };
    let install_id = installed["install_id"]
        .as_str()
        .ok_or_else(|| Error::internal("install returned no install_id"))?
        .to_owned();
    let digest = installed["digest"]
        .as_str()
        .ok_or_else(|| Error::internal("install returned no digest"))?
        .to_owned();
    // Sandbox auto-approval: the exact installed digest only.
    let approved = client::rpc_state(
        &sb.state_dir(),
        "app_local_install_approve",
        json!({"install_id": install_id, "digest": digest}),
    )?;

    let created_context = client::rpc_state(
        &sb.state_dir(),
        "app_context_create",
        json!({
            "install_id": install_id,
            "label": context,
            "input_defaults": {},
            "request_id": "dev-context",
        }),
    )?;
    let context_id = created_context["context"]["id"]
        .as_str()
        .ok_or_else(|| Error::internal("context create returned no id"))?
        .to_owned();

    // The host keeps one connection per provider/account, so a combined
    // `provider.token` (read + draft) is the default. A read-only fallback
    // still binds source; image/writer then fail at the provider with the
    // missing scope rather than refusing the second enrollment here.
    let token = profile
        .credentials
        .provider
        .as_deref()
        .unwrap_or(&profile.credentials.read);
    let provider = enroll(&sb, &profile, &["provider.read", "provider.draft"], token)?;

    let mut bindings = serde_json::Map::new();
    for slot in ["source", "image", "writer"] {
        if !manifest.capabilities.contains_key(slot) {
            continue;
        }
        let binding = client::rpc_state(
            &sb.state_dir(),
            "app_binding_create",
            json!({
                "install_id": install_id,
                "context_id": context_id,
                "slot": slot,
                "connection_id": provider,
                "request_id": format!("dev-{slot}"),
            }),
        )?;
        bindings.insert(slot.to_owned(), binding);
    }

    let mut publication = Value::Null;
    let mut warnings = Vec::new();
    if publish {
        let destinations = client::rpc_state(&sb.state_dir(), "social_destinations", json!({}))?;
        let selected = destination_id.map(str::to_owned).or_else(|| {
            destinations["destinations"]
                .as_array()
                .and_then(|rows| rows.first())
                .and_then(|row| row["destination_id"].as_str())
                .map(str::to_owned)
        });
        match selected {
            Some(destination_id) => {
                let existing = client::rpc_state(
                    &sb.state_dir(),
                    "app_binding_list",
                    json!({"install_id": install_id, "context_id": context_id}),
                )?;
                let revision = existing["bindings"]
                    .as_array()
                    .and_then(|rows| {
                        // CAD-1939: that destination's own binding.
                        rows.iter().find(|row| {
                            row["slot"].as_str() == Some("publication")
                                && row["state"] == "configured"
                                && row["config"]["publish"]["destination_id"].as_str()
                                    == Some(destination_id.as_str())
                        })
                    })
                    .and_then(|row| row["revision"].as_u64());
                let mut params = json!({
                    "install_id": install_id,
                    "context_id": context_id,
                    "destination_id": destination_id,
                    "request_id": "dev-publication",
                    "timezone": timezone,
                });
                if let Some(revision) = revision {
                    params["expected_revision"] = json!(revision);
                }
                let binding =
                    client::rpc_state(&sb.state_dir(), "app_binding_use_destination", params)?;
                bindings.insert("publication".to_owned(), binding.clone());
                publication = binding;
            }
            None => warnings
                .push("no connected Instagram destination; publication binding not set".to_owned()),
        }
    }

    Ok(json!({
        "schema": 1,
        "sandbox": sb.name,
        "install_id": install_id,
        "digest": digest,
        "approval": approved,
        "context_id": context_id,
        "connections": {"provider": provider},
        "bindings": bindings,
        "publication": publication,
        "warnings": warnings,
    }))
}

fn approve_agenticos(
    name: Option<&str>,
    install_id: &str,
    context_id: Option<&str>,
    dry_run: bool,
) -> Result<Value> {
    let sb = agenticos_sandbox(name)?;
    require_marker(&sb)?;
    if dev_owner(&sb.state_dir())?.is_none() {
        return Err(Error::rejected(
            "dev agenticos approve only runs in a dev store",
        ));
    }
    crate::proto::identifier(install_id, "installation ID")?;
    let mut run_filter = json!({"install_id": install_id});
    let mut effect_filter = json!({"install_id": install_id});
    if let Some(context) = context_id {
        crate::proto::identifier(context, "context ID")?;
        run_filter["context_id"] = json!(context);
        effect_filter["context_id"] = json!(context);
    }
    effect_filter["state"] = json!("waiting");
    if dry_run {
        effect_filter["read_only"] = json!(true);
    }
    let runs = client::rpc_state(&sb.state_dir(), "app_run_list", run_filter)?;
    let pending_runs = pending_approvals(&runs, &json!({"effects": []}));
    let run_candidates = pending_runs
        .runs
        .iter()
        .map(|(id, digest)| json!({"id": id, "digest": digest}))
        .collect::<Vec<_>>();
    let mut approved_runs = Vec::new();
    if !dry_run {
        for (id, digest) in &pending_runs.runs {
            approved_runs.push(client::rpc_state(
                &sb.state_dir(),
                "app_run_approve",
                json!({"run_id": id, "digest": digest}),
            )?);
        }
    }
    let mut effect_candidates = Vec::new();
    let mut approved_effects = Vec::new();
    let mut skipped_effects = Vec::new();
    let mut warnings = Vec::new();
    // `app_effect_list` returns the daemon's 100-row window. Dry-run pages
    // it read-only with `offset`; live mode approves each batch and
    // re-queries, so later waiting rows become the next window.
    let mut offset = 0;
    for batch in 0..1_000 {
        if dry_run {
            effect_filter["offset"] = json!(offset);
        }
        let effects = client::rpc_state(&sb.state_dir(), "app_effect_list", effect_filter.clone())?;
        let visible = effects["effects"].as_array().map_or(0, Vec::len);
        let (pending, mut skipped) = pending_effect_rows(&effects);
        skipped_effects.append(&mut skipped);
        if !dry_run && pending.is_empty() {
            break;
        }
        effect_candidates.extend(
            pending
                .iter()
                .map(|(id, digest)| json!({"id": id, "digest": digest})),
        );
        if dry_run {
            if visible < 100 {
                break;
            }
            offset += 100;
            continue;
        }
        for (id, digest) in &pending {
            approved_effects.push(client::rpc_state(
                &sb.state_dir(),
                "app_effect_decide",
                json!({"effect_id": id, "digest": digest, "decision": "accept", "execute": false}),
            )?);
        }
        if batch == 999 {
            warnings.push("stopped after 1000 effect batches; rerun to continue".to_owned());
        }
    }
    if dry_run && offset >= 100_000 {
        warnings.push("stopped after 1000 effect pages; rerun to continue".to_owned());
    }
    Ok(json!({
        "schema": 1,
        "sandbox": sb.name,
        "install_id": install_id,
        "context_id": context_id,
        "dry_run": dry_run,
        "runs": {
            "selected": pending_runs.runs.len(),
            "candidates": run_candidates,
            "approved": approved_runs,
            "skipped": pending_runs.skipped_runs,
        },
        "effects": {
            "selected": effect_candidates.len(),
            "candidates": effect_candidates,
            "approved": approved_effects,
            "skipped": skipped_effects,
        },
        "warnings": warnings,
    }))
}

pub(super) struct PendingApprovals {
    pub(super) runs: Vec<(String, String)>,
    pub(super) effects: Vec<(String, String)>,
    pub(super) skipped_runs: Vec<Value>,
    pub(super) skipped_effects: Vec<Value>,
}

pub(super) fn pending_approvals(runs: &Value, effects: &Value) -> PendingApprovals {
    let mut pending = PendingApprovals {
        runs: Vec::new(),
        effects: Vec::new(),
        skipped_runs: Vec::new(),
        skipped_effects: Vec::new(),
    };
    for run in runs["runs"].as_array().map_or(&[][..], Vec::as_slice) {
        if run["state"].as_str() != Some("awaiting_approval") {
            continue;
        }
        match (run["id"].as_str(), run["snapshot_digest"].as_str()) {
            (Some(id), Some(digest)) => pending.runs.push((id.to_owned(), digest.to_owned())),
            _ => pending.skipped_runs.push(json!({
                "id": run["id"],
                "reason": "awaiting run has no id or snapshot digest",
            })),
        }
    }
    let (effects, skipped) = pending_effect_rows(effects);
    pending.effects = effects;
    pending.skipped_effects = skipped;
    pending
}

fn pending_effect_rows(effects: &Value) -> (Vec<(String, String)>, Vec<Value>) {
    let mut pending = Vec::new();
    let mut skipped = Vec::new();
    for effect in effects["effects"].as_array().map_or(&[][..], Vec::as_slice) {
        if effect["state"].as_str() != Some("waiting") {
            continue;
        }
        let id = effect["effect_id"]
            .as_str()
            .or_else(|| effect["id"].as_str());
        match (id, effect["digest"].as_str()) {
            (Some(id), Some(digest)) => pending.push((id.to_owned(), digest.to_owned())),
            _ => skipped.push(json!({
                "id": effect["effect_id"],
                "reason": "waiting effect has no id or digest",
            })),
        }
    }
    (pending, skipped)
}

fn enroll(
    sb: &Sandbox,
    profile: &provider::Profile,
    scopes: &[&str],
    path: &Path,
) -> Result<String> {
    let token = std::fs::read_to_string(path)
        .map_err(|_| Error::rejected("provider credential is unreadable"))?;
    let enrolled = client::rpc_state(
        &sb.state_dir(),
        "connection_create",
        json!({
            "provider": "agenticos_external",
            "account": profile.workspace,
            "shape": "token",
            "token": token.trim(),
            "scopes": scopes,
            "accept_same_uid_risk": true,
        }),
    )?;
    enrolled["connection"]["id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::internal("connection create returned no id"))
}
