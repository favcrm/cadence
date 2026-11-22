//! Shared hosted claim context and delivery. Event receipt alone never calls this module.
use super::reporting::MAX_PRIVATE_APP_ZIP_BYTES;
use super::Kickoff;
use crate::error::{Error, Result};
use crate::private_app_update::{
    format_readiness_refusal, resolve_private_app_updater, valid_zip_file_name,
    PrivateAppAdapterKind, PrivateAppReadinessReason, PrivateAppUpdaterCandidate,
    PrivateAppUpdaterResolution, ResolvedPrivateAppUpdater, ServerPrivateAppReadiness,
};
use crate::remote_cli::{self, RemoteAnswer, RemoteTarget};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default, Deserialize)]
struct PrivateAppUpdateFrontmatter {
    #[serde(default)]
    artifact_id: Option<String>,
    #[serde(default)]
    version: Option<u64>,
    #[serde(default)]
    install_id: Option<String>,
    #[serde(default)]
    expected_digest: Option<String>,
    #[serde(default)]
    expected_generation: Option<String>,
    #[serde(default)]
    updater_agent: Option<String>,
    #[serde(default)]
    dedicated_updater: Option<String>,
    #[serde(default)]
    workspace_default_updater: Option<String>,
    #[serde(default)]
    default_app_agent: Option<String>,
    #[serde(default)]
    is_chat_default_only: Option<bool>,
    #[serde(default)]
    personal_assistant_requested: Option<bool>,
    #[serde(default)]
    workspace_mcp_paired: Option<bool>,
    #[serde(default)]
    board_active: Option<bool>,
    #[serde(default)]
    server_readiness: Option<ServerPrivateAppReadiness>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ArtifactCandidate {
    id: String,
    version: Option<u64>,
    name: Option<String>,
}

struct StagedPrivateAppPackage {
    artifact_id: String,
    file_name: String,
    zip_sha256: String,
    bytes: Vec<u8>,
    meta: PrivateAppUpdateFrontmatter,
    updater: ResolvedPrivateAppUpdater,
}

/// Destination for this kickoff's context and handoff.
/// Org, endpoint, and workspace stay the invocation's original selection.
/// The member pin is the claimant captured when the coordinator opened.
/// A missing or one-sided pin is unavailable: the current saved login is
/// never substituted.
pub fn claimant_target(target: &RemoteTarget, kickoff: &Kickoff) -> Result<RemoteTarget> {
    let Some(subject_id) = kickoff.subject_id.filter(|value| !value.is_empty()) else {
        return Err(Error::rejected(
            "hosted assignment has no original member; context was not read and the local agent was not messaged",
        ));
    };
    let Some(issuer) = kickoff.issuer.filter(|value| !value.is_empty()) else {
        return Err(Error::rejected(
            "hosted assignment has no original issuer; context was not read and the local agent was not messaged",
        ));
    };
    Ok(RemoteTarget {
        org: target.org.clone(),
        endpoint: target.endpoint.clone(),
        org_id: target.org_id.clone(),
        identity: Some(remote_cli::CliIdentityPin {
            subject_id: subject_id.to_string(),
            issuer: issuer.to_string(),
        }),
    })
}

/// Reload the saved login against the claimant pin before a handoff that
/// does not itself open a remote verb. Renewal of that same member is
/// allowed; a different member or issuer fails inside credential load,
/// before any request is given that login's authority.
fn reload_claimant(target: &RemoteTarget) -> Result<()> {
    let dir = crate::remote_auth::auth_dir(None).map_err(|_| {
        Error::rejected(
            "remote credential directory is unavailable; the local agent was not messaged",
        )
    })?;
    remote_cli::cli_material(&dir, target)?;
    Ok(())
}

fn lookup_local_adapter(state: &Path, alias: &str) -> Option<String> {
    let shown = crate::client::rpc(state, "agent_show", json!({ "alias": alias })).ok()?;
    let agent = shown.get("agent")?;
    let provider = agent.get("provider").and_then(Value::as_str)?;
    let endpoint_kind = agent.get("endpoint_kind").and_then(Value::as_str)?;
    Some(format!("{provider}/{endpoint_kind}"))
}

pub fn deliver_kickoff(
    target: &RemoteTarget,
    local_state: Option<PathBuf>,
    kickoff: &Kickoff,
    wake_timeout: u64,
) -> Result<Value> {
    let state = match local_state {
        Some(state) => state,
        None => crate::client::state_dir()?,
    };
    deliver_kickoff_with_adapter(
        target,
        &state,
        kickoff,
        wake_timeout,
        &|| lookup_local_adapter(&state, kickoff.alias),
        &|params| crate::client::rpc(&state, "agent_send", params),
    )
}

/// The daemon uses its existing non-steering send path, not an invented
/// operator connection. CLI delivery uses the normal agent_send RPC above.
pub fn deliver_kickoff_with(
    target: &RemoteTarget,
    state: &Path,
    kickoff: &Kickoff,
    wake_timeout: u64,
    send: &dyn Fn(Value) -> Result<Value>,
) -> Result<Value> {
    deliver_kickoff_with_adapter(target, state, kickoff, wake_timeout, &|| None, send)
}

/// Deliver a hosted kickoff with an optional adapter resolver for pre-dispatch
/// private-app readiness verification and turn-bound binary ZIP custody.
pub fn deliver_kickoff_with_adapter(
    target: &RemoteTarget,
    state: &Path,
    kickoff: &Kickoff,
    wake_timeout: u64,
    adapter: &dyn Fn() -> Option<String>,
    send: &dyn Fn(Value) -> Result<Value>,
) -> Result<Value> {
    let target = claimant_target(target, kickoff)?;
    let (text, note) = if let Some(text) =
        crate::local_assignment::reporting::stored_kickoff_text(state, kickoff)?
    {
        reload_claimant(&target)?;
        if text.contains("Private app package_ref: ") {
            let has_pkg = crate::local_assignment::reporting::hosted_binding_target(
                state,
                kickoff.message_id,
            )?
            .and_then(|binding| binding.private_app_package)
            .is_some();
            if !has_pkg {
                return Err(Error::rejected(
                    "private app kickoff binding lost its package custody; refusing blind replay",
                ));
            }
        }
        (text, context_note_path(state, kickoff))
    } else {
        let mut context = require_context(&target, kickoff, wake_timeout)?;
        let staged =
            prepare_private_app_kickoff(&target, kickoff, wake_timeout, adapter, &mut context)?;
        let note = write_note(state, kickoff, &context)?;
        let text = kickoff_line(kickoff, &note, &context, staged.as_ref())?;
        crate::local_assignment::reporting::store_kickoff_binding(
            state,
            &target.org,
            &target.endpoint,
            kickoff,
            &text,
        )?;
        if let Some(pkg) = staged.as_ref() {
            crate::local_assignment::reporting::store_private_app_package(
                state,
                kickoff.message_id,
                &pkg.artifact_id,
                &pkg.file_name,
                &pkg.bytes,
                pkg.meta.install_id.as_deref(),
                pkg.meta.expected_digest.as_deref(),
                pkg.meta.expected_generation.as_deref(),
            )?;
        }
        (text, note)
    };
    let mut params = json!({
        "alias": kickoff.alias,
        "text": text,
        "message": kickoff.message_id,
    });
    if let Some(reply_to) = std::env::var("CADENCE_ALIAS")
        .ok()
        .filter(|alias| crate::proto::identifier(alias, "alias").is_ok())
    {
        params["reply_to"] = json!(reply_to);
    }
    reload_claimant(&target)?;
    let mut sent = send(params)?;
    sent["note"] = json!(note.display().to_string());
    Ok(sent)
}

fn valid_artifact_ref(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.starts_with("art-")
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn sanitize_zip_file_name(raw: Option<&str>, artifact_id: &str) -> String {
    if let Some(name) = raw {
        let trimmed = name.trim();
        if valid_zip_file_name(trimmed) {
            return trimmed.to_string();
        }
    }
    format!("{artifact_id}.zip")
}

/// Extract `(artifact_id, optional_version)` from a hosted linked artifact name
/// such as `hosted:My-App--art-0123456789abcdef-v1.zip` or `art-...zip`.
fn parse_linked_zip_artifact_name(name: &str) -> Option<(String, Option<u64>)> {
    if !name.ends_with(".zip") {
        return None;
    }
    let stem = name.strip_suffix(".zip")?;
    let art_part = if let Some((_, tail)) = stem.rsplit_once("--art-") {
        format!("art-{tail}")
    } else if stem.starts_with("art-") {
        stem.to_string()
    } else {
        return None;
    };
    if let Some((base, ver_str)) = art_part.rsplit_once("-v") {
        if let Ok(ver) = ver_str.parse::<u64>() {
            if valid_artifact_ref(base) && ver > 0 {
                return Some((base.to_string(), Some(ver)));
            }
        }
    }
    valid_artifact_ref(&art_part).then_some((art_part, None))
}

fn probe_private_app_artifact_kind(
    target: &RemoteTarget,
    artifact_id: &str,
    expected_version: Option<u64>,
    deadline: Instant,
    wake_timeout: u64,
) -> Result<Option<ArtifactCandidate>> {
    let mut args = Map::new();
    args.insert("id".into(), json!(artifact_id));
    let answer = remote_cli::call_verb(
        target,
        "artifact_show",
        args,
        deadline,
        wake_timeout,
        |_| {},
    )?;
    let RemoteAnswer::Ok(body) = answer else {
        return Err(Error::rejected(
            "artifact metadata lookup answered a conflict",
        ));
    };
    let Some(versions) = body.get("versions").and_then(Value::as_array) else {
        return Ok(None);
    };
    let matched = versions.iter().find(|row| {
        row.get("id").and_then(Value::as_str) == Some(artifact_id)
            && expected_version
                .is_none_or(|ver| row.get("version").and_then(Value::as_u64) == Some(ver))
    });
    let Some(entry) = matched else {
        return Ok(None);
    };
    if entry.get("kind").and_then(Value::as_str) != Some("private-app-zip") {
        return Ok(None);
    }
    let version = entry
        .get("version")
        .and_then(Value::as_u64)
        .or(expected_version);
    let name = entry
        .get("name")
        .or_else(|| entry.get("title"))
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(Some(ArtifactCandidate {
        id: artifact_id.to_string(),
        version,
        name,
    }))
}

fn collect_private_app_artifacts(
    context: &Value,
    target: &RemoteTarget,
    deadline: Instant,
    wake_timeout: u64,
) -> Result<Vec<ArtifactCandidate>> {
    let issue_envelope = &context["issue"];
    let issue_obj = issue_envelope
        .get("issue")
        .filter(|v| v.is_object())
        .unwrap_or(issue_envelope);
    let items = issue_envelope
        .get("artifacts")
        .and_then(Value::as_array)
        .or_else(|| issue_obj.get("artifacts").and_then(Value::as_array));
    let Some(items) = items else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for item in items {
        let Some(obj) = item.as_object() else {
            continue;
        };
        if let Some(id) = obj
            .get("id")
            .or_else(|| obj.get("artifact_id"))
            .and_then(Value::as_str)
            .filter(|id| valid_artifact_ref(id))
        {
            match obj.get("kind").and_then(Value::as_str) {
                Some("private-app-zip") => {
                    out.push(ArtifactCandidate {
                        id: id.to_string(),
                        version: obj.get("version").and_then(Value::as_u64),
                        name: obj.get("name").and_then(Value::as_str).map(str::to_string),
                    });
                }
                None => {
                    if let Some(found) = probe_private_app_artifact_kind(
                        target,
                        id,
                        obj.get("version").and_then(Value::as_u64),
                        deadline,
                        wake_timeout,
                    )? {
                        out.push(found);
                    }
                }
                Some(_) => {}
            }
            continue;
        }
        if let Some(name) = obj.get("name").and_then(Value::as_str) {
            if let Some((id, ver)) = parse_linked_zip_artifact_name(name) {
                if let Some(mut found) =
                    probe_private_app_artifact_kind(target, &id, ver, deadline, wake_timeout)?
                {
                    if found.name.is_none() {
                        found.name = Some(name.to_string());
                    }
                    if !out.iter().any(|existing| existing.id == found.id) {
                        out.push(found);
                    }
                }
            }
        }
    }
    Ok(out)
}

fn fetch_profile_enabled_tools(
    target: &RemoteTarget,
    actor: &str,
    deadline: Instant,
    wake_timeout: u64,
) -> Option<Vec<String>> {
    let profile_id = actor.strip_prefix("agents/")?;
    if !profile_id.starts_with("ap_") {
        return None;
    }
    let mut args = Map::new();
    args.insert("id".into(), json!(profile_id));
    let Ok(RemoteAnswer::Ok(body)) = remote_cli::call_verb(
        target,
        "agent_profile_show",
        args,
        deadline,
        wake_timeout,
        |_| {},
    ) else {
        return None;
    };
    let tools = body
        .pointer("/profile/enabled_tools")
        .or_else(|| body.get("enabled_tools"))
        .and_then(Value::as_array)?;
    Some(
        tools
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
    )
}

fn non_empty_trimmed(opt: Option<&str>) -> Option<&str> {
    opt.map(str::trim).filter(|s| !s.is_empty())
}

fn canonical_agent_actor(raw: &str) -> String {
    if raw.starts_with("agents/") || raw.starts_with("service/") {
        raw.to_string()
    } else if raw.starts_with("ap_") {
        format!("agents/{raw}")
    } else {
        raw.to_string()
    }
}

fn make_updater_candidate<'a>(
    actor_opt: Option<&'a str>,
    fm_is_chat_default_only: bool,
    chat_default_actor: Option<&str>,
    selected_actor_for_tools: Option<&str>,
    enabled_tools: Option<&'a [String]>,
    adapter: PrivateAppAdapterKind,
    fm: &'a PrivateAppUpdateFrontmatter,
) -> Option<PrivateAppUpdaterCandidate<'a>> {
    actor_opt.map(|actor| PrivateAppUpdaterCandidate {
        actor: Some(actor),
        is_chat_default_only: fm_is_chat_default_only || chat_default_actor == Some(actor),
        capabilities: None,
        enabled_tools: if selected_actor_for_tools == Some(actor) {
            enabled_tools
        } else {
            None
        },
        adapter,
        workspace_mcp_paired: fm.workspace_mcp_paired,
        board_active: fm.board_active,
        server_readiness: fm.server_readiness.as_ref(),
    })
}

fn resolve_updater_before_download(
    target: &RemoteTarget,
    kickoff: &Kickoff,
    fm: &PrivateAppUpdateFrontmatter,
    adapter: &dyn Fn() -> Option<String>,
    deadline: Instant,
    wake_timeout: u64,
) -> Result<ResolvedPrivateAppUpdater> {
    let adapter_str = adapter();
    let adapter_kind = PrivateAppAdapterKind::from_endpoint_adapter(adapter_str.as_deref());

    // Never promote a claimed profile or CAD-2434 chat-only default into an updater.
    let explicit_actor = if let Some(agent) = non_empty_trimmed(fm.updater_agent.as_deref()) {
        Some(canonical_agent_actor(agent))
    } else if fm.personal_assistant_requested == Some(true) {
        Some("service/personal_assistant".to_string())
    } else {
        None
    };
    let dedicated_actor =
        non_empty_trimmed(fm.dedicated_updater.as_deref()).map(canonical_agent_actor);
    let workspace_default_actor =
        non_empty_trimmed(fm.workspace_default_updater.as_deref()).map(canonical_agent_actor);
    let chat_default_actor =
        non_empty_trimmed(fm.default_app_agent.as_deref()).map(canonical_agent_actor);

    // Fetch only the selected updater profile's own opt-in scopes, never the dispatcher login's.
    let selected_actor_for_tools = explicit_actor
        .as_deref()
        .or(dedicated_actor.as_deref())
        .or(workspace_default_actor.as_deref());
    let profile_enabled_tools = selected_actor_for_tools
        .and_then(|actor| fetch_profile_enabled_tools(target, actor, deadline, wake_timeout));
    let enabled_tools_slice = profile_enabled_tools.as_deref();
    let fm_chat_default = fm.is_chat_default_only == Some(true);

    let resolution = PrivateAppUpdaterResolution {
        explicit_agent: make_updater_candidate(
            explicit_actor.as_deref(),
            fm_chat_default,
            chat_default_actor.as_deref(),
            selected_actor_for_tools,
            enabled_tools_slice,
            adapter_kind,
            fm,
        ),
        dedicated_app_updater: make_updater_candidate(
            dedicated_actor.as_deref(),
            fm_chat_default,
            chat_default_actor.as_deref(),
            selected_actor_for_tools,
            enabled_tools_slice,
            adapter_kind,
            fm,
        ),
        workspace_default_updater: make_updater_candidate(
            workspace_default_actor.as_deref(),
            fm_chat_default,
            chat_default_actor.as_deref(),
            selected_actor_for_tools,
            enabled_tools_slice,
            adapter_kind,
            fm,
        ),
        has_chat_default_app_agent: chat_default_actor.is_some(),
        chat_default_actor: chat_default_actor.as_deref(),
        personal_assistant_requested: fm.personal_assistant_requested == Some(true),
    };

    let resolved = resolve_private_app_updater(&resolution).map_err(|reasons| {
        Error::invalid(
            "private_app_readiness_refused",
            format_readiness_refusal(&reasons),
        )
    })?;
    let Some(claimed_profile) = non_empty_trimmed(kickoff.profile_id) else {
        return Err(Error::invalid(
            "private_app_readiness_refused",
            format_readiness_refusal(&[PrivateAppReadinessReason::ActorMismatch]),
        ));
    };
    if resolved.actor != canonical_agent_actor(claimed_profile) {
        return Err(Error::invalid(
            "private_app_readiness_refused",
            format_readiness_refusal(&[PrivateAppReadinessReason::ActorMismatch]),
        ));
    }
    Ok(resolved)
}

fn download_private_app_zip(
    target: &RemoteTarget,
    artifact_id: &str,
    requested_version: Option<u64>,
    fallback_name: Option<&str>,
    deadline: Instant,
    wake_timeout: u64,
) -> Result<(Vec<u8>, String, String)> {
    fn malformed() -> Error {
        Error::rejected(
            "private app artifact download returned malformed or inconsistent data; local agent was not messaged",
        )
    }

    let mut bytes = Vec::new();
    let mut pinned_version = requested_version;
    let mut pinned_total: Option<usize> = None;
    let mut pinned_digest: Option<String> = None;

    let file_meta = loop {
        let mut args = Map::new();
        args.insert("id".into(), json!(artifact_id));
        args.insert("offset".into(), json!(bytes.len()));
        if let Some(ver) = pinned_version {
            args.insert("version".into(), json!(ver));
        }
        let RemoteAnswer::Ok(mut body) = remote_cli::call_verb(
            target,
            "artifact_read",
            args,
            deadline,
            wake_timeout,
            |_| {},
        )?
        else {
            return Err(Error::rejected(
                "private app artifact read answered a conflict",
            ));
        };
        let metadata = body
            .get("file")
            .and_then(Value::as_object)
            .ok_or_else(malformed)?;
        if metadata.get("id").and_then(Value::as_str) != Some(artifact_id) {
            return Err(malformed());
        }
        if metadata.get("kind").and_then(Value::as_str) != Some("private-app-zip") {
            return Err(Error::rejected(
                "assigned artifact is not a private-app-zip package; local agent was not messaged",
            ));
        }
        let page_version = metadata
            .get("version")
            .and_then(Value::as_u64)
            .filter(|v| *v > 0 && *v <= 9_007_199_254_740_991)
            .ok_or_else(malformed)?;
        if pinned_version.is_some_and(|v| v != page_version) {
            return Err(malformed());
        }
        pinned_version = Some(page_version);
        let total = body
            .get("total")
            .and_then(Value::as_u64)
            .filter(|t| *t > 0 && *t <= MAX_PRIVATE_APP_ZIP_BYTES as u64)
            .and_then(|t| usize::try_from(t).ok())
            .ok_or_else(malformed)?;
        if pinned_total.is_some_and(|expected| expected != total)
            || body.get("offset").and_then(Value::as_u64) != Some(bytes.len() as u64)
            || metadata
                .get("bytes")
                .is_some_and(|size| size.as_u64() != Some(total as u64))
        {
            return Err(malformed());
        }
        pinned_total = Some(total);
        let digest = metadata
            .get("digest")
            .and_then(Value::as_str)
            .filter(|d| {
                d.len() == 64
                    && d.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            .ok_or_else(malformed)?;
        if let Some(expected) = &pinned_digest {
            if expected != digest {
                return Err(malformed());
            }
        } else {
            pinned_digest = Some(digest.to_string());
        }
        let encoded = body
            .get("content_base64")
            .and_then(Value::as_str)
            .ok_or_else(malformed)?;
        let remaining = total.checked_sub(bytes.len()).ok_or_else(malformed)?;
        if encoded.len() > remaining.div_ceil(3) * 4 {
            return Err(malformed());
        }
        let prev_len = bytes.len();
        base64::engine::general_purpose::STANDARD
            .decode_vec(encoded, &mut bytes)
            .map_err(|_| malformed())?;
        if bytes.len() > total || (bytes.len() == prev_len && bytes.len() < total) {
            return Err(malformed());
        }
        if bytes.len() == total {
            break body["file"].take();
        }
    };

    if !(bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06")) {
        return Err(Error::rejected(
            "downloaded private app package is not a valid ZIP archive; local agent was not messaged",
        ));
    }

    let expected_hex = pinned_digest.ok_or_else(malformed)?;
    let actual_hex = {
        use std::fmt::Write as _;
        let mut s = String::with_capacity(64);
        for b in Sha256::digest(&bytes) {
            let _ = write!(&mut s, "{b:02x}");
        }
        s
    };
    if actual_hex != expected_hex {
        return Err(Error::rejected(
            "downloaded private app artifact bytes do not match digest; local agent was not messaged",
        ));
    }

    let raw_name = file_meta
        .get("name")
        .or_else(|| file_meta.get("title"))
        .and_then(Value::as_str)
        .or(fallback_name);
    let file_name = sanitize_zip_file_name(raw_name, artifact_id);
    Ok((bytes, format!("sha256:{actual_hex}"), file_name))
}

fn prepare_private_app_kickoff(
    target: &RemoteTarget,
    kickoff: &Kickoff,
    wake_timeout: u64,
    adapter: &dyn Fn() -> Option<String>,
    context: &mut Value,
) -> Result<Option<StagedPrivateAppPackage>> {
    let issue_envelope = &context["issue"];
    let issue_obj = issue_envelope
        .get("issue")
        .filter(|v| v.is_object())
        .unwrap_or(issue_envelope);
    let fm_value = issue_obj
        .pointer("/frontmatter/private_app_update")
        .or_else(|| issue_envelope.pointer("/frontmatter/private_app_update"));
    let has_frontmatter = fm_value.is_some_and(|v| !v.is_null());

    let fm = match fm_value {
        None | Some(Value::Null) => PrivateAppUpdateFrontmatter::default(),
        Some(v) if v.is_object() => {
            serde_json::from_value::<PrivateAppUpdateFrontmatter>(v.clone())
                .map_err(|_| Error::rejected("invalid private_app_update frontmatter on issue"))?
        }
        Some(_) => {
            return Err(Error::rejected(
                "private_app_update frontmatter must be an object",
            ))
        }
    };

    let Some(deadline) = Instant::now().checked_add(Duration::from_secs(wake_timeout)) else {
        return Err(Error::rejected(
            "invalid wake timeout; local agent was not messaged",
        ));
    };

    let candidates = collect_private_app_artifacts(context, target, deadline, wake_timeout)?;
    if !has_frontmatter && candidates.is_empty() {
        return Ok(None);
    }

    let selected: Option<ArtifactCandidate> = if let Some(explicit_art) = fm.artifact_id.as_deref()
    {
        if !valid_artifact_ref(explicit_art) {
            return Err(Error::rejected(
                "private_app_update.artifact_id is invalid; local agent was not messaged",
            ));
        }
        Some(
            candidates
                .into_iter()
                .find(|c| c.id == explicit_art)
                .unwrap_or(ArtifactCandidate {
                    id: explicit_art.to_string(),
                    version: fm.version,
                    name: None,
                }),
        )
    } else if candidates.len() == 1 {
        candidates.into_iter().next()
    } else if candidates.len() > 1 {
        return Err(Error::rejected(
            "multiple private-app-zip artifacts are attached; specify private_app_update.artifact_id",
        ));
    } else {
        None
    };

    let updater =
        resolve_updater_before_download(target, kickoff, &fm, adapter, deadline, wake_timeout)?;

    let Some(art) = selected else {
        return Ok(None);
    };

    let version = fm.version.or(art.version);
    let (bytes, zip_sha256, file_name) = download_private_app_zip(
        target,
        &art.id,
        version,
        art.name.as_deref(),
        deadline,
        wake_timeout,
    )?;

    let mut pkg_note = json!({
        "package_ref": art.id,
        "zip_sha256": zip_sha256,
        "file_name": file_name,
        "updater_actor": updater.actor,
        "updater_source": updater.source,
    });
    if let Some(install_id) = fm.install_id.as_deref() {
        pkg_note["install_id"] = json!(install_id);
    }
    if let Some(expected_digest) = fm.expected_digest.as_deref() {
        pkg_note["expected_digest"] = json!(expected_digest);
    }
    if let Some(expected_generation) = fm.expected_generation.as_deref() {
        pkg_note["expected_generation"] = json!(expected_generation);
    }
    context["private_app_package"] = pkg_note;

    Ok(Some(StagedPrivateAppPackage {
        artifact_id: art.id,
        file_name,
        zip_sha256,
        bytes,
        meta: fm,
        updater,
    }))
}

pub fn require_context(
    target: &RemoteTarget,
    kickoff: &Kickoff,
    wake_timeout: u64,
) -> Result<Value> {
    let context = issue_context(target, kickoff, wake_timeout);
    if context.get("context_error").is_some() {
        return Err(Error::rejected(
            "hosted issue context was not read; the local agent was not messaged and the assignment was not left running",
        ));
    }
    Ok(context)
}

// A reassignment must not rewrite a snapshot an older local actor can still read.
fn context_note_path(dir: &Path, kickoff: &Kickoff) -> PathBuf {
    dir.join("hosted-assignments")
        .join(format!("{}.md", kickoff.message_id))
}

pub fn write_note(dir: &Path, kickoff: &Kickoff, context: &Value) -> Result<PathBuf> {
    let message = kickoff.message_id;
    if message.is_empty()
        || message.contains('/')
        || message.contains('\\')
        || message.contains('\0')
    {
        return Err(Error::rejected(
            "message id cannot name a local note; the actor was not messaged",
        ));
    }
    let folder = dir.join("hosted-assignments");
    fs::create_dir_all(&folder)?;
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o700))?;
    let path = context_note_path(dir, kickoff);
    if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(Error::rejected(
            "assignment note path is a symlink; the actor was not messaged",
        ));
    }
    let serialized = serde_json::to_string_pretty(context).map_err(|error| {
        Error::rejected(format!("hosted context could not be serialized: {error}"))
    })?;
    let body = redact_secrets(serialized);
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
    }
    Ok(path)
}

fn kickoff_line(
    kickoff: &Kickoff,
    note: &Path,
    context: &Value,
    staged: Option<&StagedPrivateAppPackage>,
) -> Result<String> {
    let title = context
        .pointer("/issue_title")
        .and_then(Value::as_str)
        .unwrap_or("hosted issue");
    let title: String = title
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(180)
        .collect();
    let mut line = format!(
        "Hosted issue {} ({title}). Read {} for this assignment's issue, comments, and wiki index. Use its org selector explicitly for hosted ticket/wiki actions, verify organization_id and audience, and do not change global org selection. Worktree: {}. A managed endpoint ends its final answer with SHA:<40-or-64-lowercase-hex>. A PTY explicit reporter uses cadence message result {} --token <turn_id> --text '<summary>' --sha <40-or-64-lowercase-hex>.",
        kickoff.issue_id,
        note.display(),
        kickoff.worktree,
        kickoff.message_id
    );
    if let Some(pkg) = staged {
        line.push_str(&format!(
            " Private app package_ref: {} ({}) for updater {}.",
            pkg.artifact_id, pkg.zip_sha256, pkg.updater.actor
        ));
    }
    if line.chars().any(char::is_control) || line.len() > 4000 {
        return Err(Error::rejected(
            "local actor message was not a single safe line; nothing was sent",
        ));
    }
    Ok(line)
}

fn issue_context(target: &RemoteTarget, kickoff: &Kickoff, wake_timeout: u64) -> Value {
    let mut show_args = Map::new();
    show_args.insert("id".into(), json!(kickoff.issue_id));
    show_args.insert("json".into(), json!(true));
    let mut history_args = Map::new();
    history_args.insert("id".into(), json!(kickoff.issue_id));
    history_args.insert("limit".into(), json!(20));
    let mut wiki_args = Map::new();
    wiki_args.insert("path".into(), json!(""));
    // One budget covers the issue, its comments, and the wiki index. A later
    // read cannot restart the wake clock after an earlier read used it up.
    let Some(deadline) = Instant::now().checked_add(Duration::from_secs(wake_timeout)) else {
        return json!({
            "org": target.org,
            "organization_id": kickoff.organization_id,
            "audience": target.endpoint,
            "assignment_id": kickoff.assignment_id,
            "claim_id": kickoff.claim_id,
            "generation": kickoff.generation,
            "context_error": "invalid wake timeout; the local agent was not messaged"
        });
    };
    let show = remote_cli::call_verb(
        target,
        "issue_show",
        show_args,
        deadline,
        wake_timeout,
        |_| {},
    );
    let history = remote_cli::call_verb(
        target,
        "issue_history",
        history_args,
        deadline,
        wake_timeout,
        |_| {},
    );
    let wiki = remote_cli::call_verb(
        target,
        "enumerate",
        wiki_args,
        deadline,
        wake_timeout,
        |_| {},
    );
    let (show_ok, issue) = outcome(show);
    let (history_ok, history) = outcome(history);
    let (wiki_ok, wiki) = outcome(wiki);
    // The hosted CLI answers `issue_show` with the issue nested under `issue`;
    // a top-level title is accepted too. Resolve the title once, here, so the
    // kickoff line never has to search the stored envelope.
    let issue_title = issue
        .pointer("/issue/title")
        .or_else(|| issue.pointer("/title"))
        .and_then(Value::as_str)
        .unwrap_or("hosted issue")
        .to_string();
    let mut context = json!({
        "org": target.org,
        "organization_id": kickoff.organization_id,
        "audience": target.endpoint,
        "assignment_id": kickoff.assignment_id,
        "claim_id": kickoff.claim_id,
        "generation": kickoff.generation,
        "issue": issue,
        "issue_title": issue_title,
        "comments": history,
        "wiki": wiki,
        "report": {
            "managed_endpoint": "end the final answer with SHA:<40-or-64-lowercase-hex>",
            "pty": {
                "command": "cadence message result",
                "message": kickoff.message_id,
                "token": "<turn_id>",
                "sha": "<40-or-64-lowercase-hex>"
            }
        }
    });
    if !show_ok || !history_ok || !wiki_ok {
        context["context_error"] = json!(
            "issue, comments, or wiki context was not fully read; the local agent was not messaged"
        );
    }
    context
}

fn outcome(answer: Result<RemoteAnswer>) -> (bool, Value) {
    match answer {
        Ok(RemoteAnswer::Ok(value)) => (true, value),
        Ok(RemoteAnswer::Conflict(_)) => (
            false,
            json!({"context_error": "hosted context conflicted; the local agent was not messaged"}),
        ),
        Err(error) => (false, json!({"context_error": brief(&error)})),
    }
}

fn brief(error: &Error) -> String {
    let text: String = error
        .to_string()
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(240)
        .collect();
    redact_secrets(text)
}

pub fn redact_secrets(text: String) -> String {
    let mut out: Option<String> = None;
    let mut copied = 0;
    for (start, _) in text.match_indices("hct_") {
        if start < copied {
            continue;
        }
        let after = &text[start + 4..];
        let length = after
            .chars()
            .take(43)
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
            .count();
        if length != 43 {
            continue;
        }
        let output = out.get_or_insert_with(|| String::with_capacity(text.len()));
        output.push_str(&text[copied..start]);
        output.push_str("hct_[redacted]");
        copied = start + 47;
    }
    match out {
        Some(mut output) => {
            output.push_str(&text[copied..]);
            output
        }
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct HomeEnvGuard {
        prev_home: Option<std::ffi::OsString>,
        prev_xdg: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl HomeEnvGuard {
        fn enter(root: &Path) -> Self {
            let lock = crate::home::CAD1922_ENV_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let prev_home = std::env::var_os("HOME");
            let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
            let config = root.join("config");
            fs::create_dir_all(&config).unwrap();
            std::env::set_var("HOME", root);
            std::env::set_var("XDG_CONFIG_HOME", &config);
            Self {
                prev_home,
                prev_xdg,
                _lock: lock,
            }
        }
    }

    impl Drop for HomeEnvGuard {
        fn drop(&mut self) {
            match &self.prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match &self.prev_xdg {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }

    fn write_login(root: &Path, endpoint: &str, caps: &[&str]) -> PathBuf {
        let auth_dir = root.join("config/cadence/remote-auth");
        fs::create_dir_all(&auth_dir).unwrap();
        fs::set_permissions(&auth_dir, fs::Permissions::from_mode(0o700)).unwrap();
        let path = auth_dir.join("cli-acme.json");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let body = json!({
            "version": "hosted-cadence-auth.v1",
            "organization_id": "org_1",
            "organization_slug": "acme",
            "audience": endpoint,
            "issuer": "https://auth.example",
            "subject_id": "sub_12345",
            "capabilities": caps,
            "access_token": format!("hct_{}", "a".repeat(43)),
            "expires_at": now + 3600,
        });
        fs::write(&path, serde_json::to_vec(&body).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        auth_dir
    }

    type RecordedRequests = Arc<Mutex<Vec<(String, Value)>>>;

    fn spawn_mock_board(
        issue_payload: Value,
        zip_bytes: Vec<u8>,
        override_digest: Option<String>,
        review_id: String,
        profile_tools: Option<Vec<String>>,
    ) -> (String, RecordedRequests, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let endpoint = format!("http://127.0.0.1:{port}");
        let mut hasher = Sha256::new();
        hasher.update(&zip_bytes);
        let computed_hex = {
            use std::fmt::Write as _;
            let mut s = String::with_capacity(64);
            for b in hasher.finalize() {
                let _ = write!(&mut s, "{b:02x}");
            }
            s
        };
        let digest_hex = override_digest.unwrap_or(computed_hex);
        let zip_sha256 = format!("sha256:{digest_hex}");
        let pkg_digest = format!("sha256:{}", "d".repeat(64));
        let recorded = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let recorded_srv = Arc::clone(&recorded);
        let art_id = format!("art-{}", "1".repeat(32));
        let total = zip_bytes.len();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&zip_bytes);

        let handle = std::thread::spawn(move || {
            for stream in listener.incoming().take(64) {
                let Ok(mut stream) = stream else { break };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let mut header_end = None;
                while header_end.is_none() {
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    header_end = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4);
                }
                let Some(hend) = header_end else { continue };
                let headers = String::from_utf8_lossy(&buf[..hend]).to_string();
                let path = headers
                    .lines()
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("")
                    .to_string();
                let content_len = headers
                    .lines()
                    .find_map(|line| {
                        let (k, v) = line.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                while buf.len() < hend + content_len {
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let body_bytes = &buf[hend..hend + content_len];
                let (status, resp_json): (u16, Value) = match path.as_str() {
                    "/__platform/cli/authorize" => (
                        200,
                        json!({"version": "hosted-cadence-cli.v1", "envelope": "hdr.pay.sig"}),
                    ),
                    "/__platform/wiki/authorize" => (
                        200,
                        json!({"version": "hosted-cadence-wiki.v1", "envelope": "hdr.pay.sig"}),
                    ),
                    "/__platform/wiki/call" => {
                        let req: Value = serde_json::from_slice(body_bytes).unwrap_or(json!({}));
                        let action = req["action"].as_str().unwrap_or("").to_string();
                        recorded_srv
                            .lock()
                            .unwrap()
                            .push((action, req["arguments"].clone()));
                        (200, json!({"entries": []}))
                    }
                    "/__platform/artifacts/upload" => (
                        200,
                        json!({"file": {"id": art_id, "version": 1, "digest": digest_hex}}),
                    ),
                    "/__platform/cli/call" => {
                        let req: Value = serde_json::from_slice(body_bytes).unwrap_or(json!({}));
                        let verb = req["verb"].as_str().unwrap_or("").to_string();
                        let args = req["arguments"].clone();
                        recorded_srv
                            .lock()
                            .unwrap()
                            .push((verb.clone(), args.clone()));
                        match verb.as_str() {
                            "issue_show" => (200, issue_payload.clone()),
                            "issue_history" => (200, json!({"comments": []})),
                            "agent_profile_show" => match &profile_tools {
                                Some(tools) => (
                                    200,
                                    json!({
                                        "profile": {
                                            "id": args["id"],
                                            "enabled_tools": tools,
                                        }
                                    }),
                                ),
                                None => (404, json!({"ok": false, "code": "not_found"})),
                            },
                            "artifact_show" => (
                                200,
                                json!({
                                    "versions": [{
                                        "id": args["id"],
                                        "version": 1,
                                        "kind": "private-app-zip",
                                        "title": "app-update.zip",
                                        "bytes": total,
                                        "digest": digest_hex,
                                    }]
                                }),
                            ),
                            "artifact_read" => (
                                200,
                                json!({
                                    "file": {
                                        "id": args["id"],
                                        "version": 1,
                                        "kind": "private-app-zip",
                                        "title": "app-update.zip",
                                        "bytes": total,
                                        "digest": digest_hex,
                                    },
                                    "offset": 0,
                                    "total": total,
                                    "content_base64": b64,
                                }),
                            ),
                            "platform_call" => {
                                let tool = args["tool"].as_str().unwrap_or("");
                                let data = match tool {
                                    "private_app_review_request" => json!({
                                        "id": review_id,
                                        "state": "pending",
                                        "outcome": "awaiting_approval",
                                        "digest": pkg_digest,
                                        "zip_sha256": zip_sha256,
                                    }),
                                    "private_app_review_status" => json!({
                                        "id": review_id,
                                        "state": "pending",
                                        "outcome": "authorized",
                                    }),
                                    "private_app_review_apply" | "private_app_review_reconcile" => {
                                        json!({
                                            "id": review_id,
                                            "state": "installed",
                                            "outcome": "applied",
                                        })
                                    }
                                    _ => json!({}),
                                };
                                (200, json!({"ok": true, "data": data}))
                            }
                            _ => (404, json!({"ok": false, "code": "not_found"})),
                        }
                    }
                    _ => (404, json!({"ok": false, "code": "not_found"})),
                };
                let body_str = resp_json.to_string();
                let response = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_str}",
                    body_str.len()
                );
                let _ = std::io::Write::write_all(&mut stream, response.as_bytes());
            }
        });
        (endpoint, recorded, handle)
    }

    #[test]
    fn private_app_dispatch_custody_mcp_lifecycle_and_settle_cleanup() {
        let root = std::env::temp_dir().join(format!("cad2584-del-e2e-{}", uuid::Uuid::new_v4()));
        let state_dir = root.join("state");
        fs::create_dir_all(&state_dir).unwrap();
        let _env = HomeEnvGuard::enter(&root);

        let art_id = format!("art-{}", "1".repeat(32));
        let zip_bytes = b"PK\x03\x04\x14\x00\x00\x00\x00\x00dispatch-custody-e2e".to_vec();
        let review_id = format!("par_{}", "f".repeat(32));
        let expected_digest = format!("sha256:{}", "e".repeat(64));
        let pkg_digest = format!("sha256:{}", "d".repeat(64));

        let issue_payload = json!({
            "issue": {
                "id": "CAD-2584",
                "title": "Update My Private App",
                "frontmatter": {
                    "private_app_update": {
                        "install_id": "install_app1",
                        "expected_digest": expected_digest,
                        "expected_generation": "c1:g1",
                        "dedicated_updater": "agents/ap_updater",
                    }
                }
            },
            "artifacts": [
                {
                    "id": art_id,
                    "kind": "private-app-zip",
                    "version": 1,
                    "name": "my-app.zip"
                }
            ]
        });

        let (endpoint, recorded, handle) = spawn_mock_board(
            issue_payload,
            zip_bytes.clone(),
            None,
            review_id.clone(),
            Some(vec!["cadence_writes".into()]),
        );
        // Dispatcher login deliberately lacks cli.write: readiness must come from the updater profile's cadence_writes.
        let auth_dir = write_login(&root, &endpoint, &["cli.read", "wiki.read"]);
        let target = RemoteTarget {
            org: "acme".into(),
            endpoint: endpoint.clone(),
            org_id: "org_1".into(),
            identity: Some(remote_cli::CliIdentityPin {
                subject_id: "sub_12345".into(),
                issuer: "https://auth.example".into(),
            }),
        };
        let message_id = "msg-delivery-e2e";
        let kickoff = Kickoff {
            alias: "updater-codex",
            issue_id: "CAD-2584",
            assignment_id: "hla_test1",
            worktree: "/tmp/wt-e2e",
            message_id,
            claim_id: "hlc_test1",
            generation: 1,
            organization_id: "org_1",
            audience: &endpoint,
            subject_id: Some("sub_12345"),
            issuer: Some("https://auth.example"),
            profile_id: Some("ap_updater"),
        };

        let delivered_text = Arc::new(Mutex::new(String::new()));
        let delivered_text_cb = Arc::clone(&delivered_text);
        let result = deliver_kickoff_with_adapter(
            &target,
            &state_dir,
            &kickoff,
            10,
            &|| Some("codex/managed".to_string()),
            &|params| {
                *delivered_text_cb.lock().unwrap() =
                    params["text"].as_str().unwrap_or("").to_string();
                Ok(json!({"message": message_id, "state": "queued"}))
            },
        )
        .unwrap();

        let text = delivered_text.lock().unwrap().clone();
        assert!(text.contains(&format!("Private app package_ref: {art_id}")));
        assert!(text.contains("agents/ap_updater"));
        let zip_path =
            crate::local_assignment::reporting::private_app_package_path(&state_dir, message_id)
                .unwrap();
        assert!(zip_path.exists());
        assert!(!text.contains(&zip_path.display().to_string()));
        let binding =
            crate::local_assignment::reporting::hosted_binding_target(&state_dir, message_id)
                .unwrap()
                .unwrap();
        let pkg = binding.private_app_package.expect("package custody bound");
        assert_eq!(pkg.artifact_ref, art_id);
        assert_eq!(pkg.file_name, "app-update.zip");

        let note_path = PathBuf::from(result["note"].as_str().unwrap());
        let note_json: Value = serde_json::from_slice(&fs::read(&note_path).unwrap()).unwrap();
        assert_eq!(note_json["private_app_package"]["package_ref"], art_id);

        // Now run Codex MCP lifecycle (prepare -> status -> apply -> reconcile) using bound custody.
        let cred_path =
            crate::local_assignment::reporting::agent_credential_path(&state_dir, message_id)
                .unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        fs::write(
            &cred_path,
            json!({
                "version": "cadence-agent-credential.v1",
                "assignment_id": "hla_test1",
                "claim_id": "hlc_test1",
                "generation": 1,
                "organization_id": "org_1",
                "audience": endpoint,
                "capabilities": ["cli.read", "cli.write"],
                "access_token": format!("hct_{}", "a".repeat(43)),
                "expires_at": now + 600,
            })
            .to_string(),
        )
        .unwrap();
        fs::set_permissions(&cred_path, fs::Permissions::from_mode(0o600)).unwrap();
        let slot = remote_cli::AgentSlot::new(
            cred_path,
            &target,
            "hla_test1".into(),
            "hlc_test1".into(),
            1,
        );

        let prepared: Value = serde_json::from_str(
            &crate::mcp_agent::invoke_private_app_tool_with(
                &auth_dir,
                &state_dir,
                message_id,
                &target,
                &slot,
                "private_app_prepare",
                &json!({"package_ref": pkg.artifact_ref}),
                10,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(prepared["data"]["id"], review_id);

        let status: Value = serde_json::from_str(
            &crate::mcp_agent::invoke_private_app_tool_with(
                &auth_dir,
                &state_dir,
                message_id,
                &target,
                &slot,
                "private_app_status",
                &json!({"id": review_id}),
                10,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(status["data"]["outcome"], "authorized");

        let applied: Value = serde_json::from_str(
            &crate::mcp_agent::invoke_private_app_tool_with(
                &auth_dir,
                &state_dir,
                message_id,
                &target,
                &slot,
                "private_app_apply",
                &json!({"id": review_id, "digest": pkg_digest, "zip_sha256": pkg.zip_sha256}),
                10,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(applied["data"]["state"], "installed");

        let reconciled: Value = serde_json::from_str(
            &crate::mcp_agent::invoke_private_app_tool_with(
                &auth_dir,
                &state_dir,
                message_id,
                &target,
                &slot,
                "private_app_reconcile",
                &json!({"id": review_id}),
                10,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(reconciled["data"]["outcome"], "applied");

        // Settle binding and confirm custody ZIP is deleted.
        crate::local_assignment::reporting::settle_binding_for_test(&state_dir, message_id)
            .unwrap();
        assert!(!zip_path.exists());

        let log = recorded.lock().unwrap();
        assert!(log.iter().any(|(v, _)| v == "agent_profile_show"));
        assert!(log.iter().any(|(v, _)| v == "artifact_read"));
        let _ = fs::remove_dir_all(&root);
        drop(handle);
    }

    #[test]
    fn private_app_pre_dispatch_negative_cases_refuse_before_send_or_custody() {
        let root = std::env::temp_dir().join(format!("cad2584-del-neg-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let _env = HomeEnvGuard::enter(&root);

        let art_id = format!("art-{}", "2".repeat(32));
        let zip_bytes = b"PK\x03\x04\x14\x00\x00\x00\x00\x00negative-test".to_vec();
        let review_id = format!("par_{}", "a".repeat(32));

        let base_issue = |fm: Value, artifacts: Value| {
            json!({
                "issue": {
                    "id": "CAD-2584",
                    "title": "Update App",
                    "frontmatter": { "private_app_update": fm }
                },
                "artifacts": artifacts,
            })
        };
        let one_art =
            json!([{"id": art_id, "kind": "private-app-zip", "version": 1, "name": "app.zip"}]);

        struct NegCase<'a> {
            label: &'a str,
            issue_payload: Value,
            adapter_val: Option<&'a str>,
            caps: &'a [&'a str],
            profile_tools: Option<Vec<&'a str>>,
            bad_digest: Option<&'a str>,
            profile_id: Option<&'a str>,
            expected_substr: &'a str,
        }

        let cases = vec![
            NegCase {
                label: "personal_assistant",
                issue_payload: base_issue(
                    json!({"updater_agent": "service/pa_1"}),
                    one_art.clone(),
                ),
                adapter_val: Some("codex/managed"),
                caps: &["cli.read", "cli.write", "wiki.read"],
                profile_tools: Some(vec!["cadence_writes"]),
                bad_digest: None,
                profile_id: None,
                expected_substr: "personal_assistant_excluded",
            },
            NegCase {
                label: "chat_default_only",
                issue_payload: base_issue(
                    json!({"default_app_agent": "agents/ap_chat"}),
                    one_art.clone(),
                ),
                adapter_val: Some("codex/managed"),
                caps: &["cli.read", "cli.write", "wiki.read"],
                profile_tools: Some(vec!["cadence_writes"]),
                bad_digest: None,
                profile_id: None,
                expected_substr: "chat_default_not_updater",
            },
            NegCase {
                label: "chat_default_claimed_as_profile",
                issue_payload: base_issue(json!({"default_app_agent": "ap_chat"}), one_art.clone()),
                adapter_val: Some("codex/managed"),
                caps: &["cli.read", "cli.write", "wiki.read"],
                profile_tools: Some(vec!["cadence_writes"]),
                bad_digest: None,
                profile_id: Some("ap_chat"),
                expected_substr: "chat_default_not_updater",
            },
            NegCase {
                label: "explicit_chat_default_only_flag",
                issue_payload: base_issue(
                    json!({"updater_agent": "agents/ap_chat", "is_chat_default_only": true}),
                    one_art.clone(),
                ),
                adapter_val: Some("codex/managed"),
                caps: &["cli.read", "cli.write", "wiki.read"],
                profile_tools: Some(vec!["cadence_writes"]),
                bad_digest: None,
                profile_id: Some("ap_chat"),
                expected_substr: "chat_default_not_updater",
            },
            NegCase {
                label: "missing_write_scope",
                issue_payload: base_issue(
                    json!({"dedicated_updater": "agents/ap_upd"}),
                    one_art.clone(),
                ),
                adapter_val: Some("codex/managed"),
                // Dispatcher login has cli.write, but updater profile lacks cadence_writes.
                caps: &["cli.read", "cli.write", "wiki.read"],
                profile_tools: Some(vec!["read_only"]),
                bad_digest: None,
                profile_id: None,
                expected_substr: "missing_write_scope",
            },
            NegCase {
                label: "unsupported_adapter",
                issue_payload: base_issue(
                    json!({"dedicated_updater": "agents/ap_upd"}),
                    one_art.clone(),
                ),
                adapter_val: Some("claude/managed"),
                caps: &["cli.read", "cli.write", "wiki.read"],
                profile_tools: Some(vec!["cadence_writes"]),
                bad_digest: None,
                profile_id: None,
                expected_substr: "adapter_upload_unsupported",
            },
            NegCase {
                label: "server_unknown_not_ready",
                issue_payload: base_issue(
                    json!({
                        "dedicated_updater": "agents/ap_upd",
                        "server_readiness": { "ready": false, "actor": "agents/ap_upd", "reasons": ["policy_locked"] }
                    }),
                    one_art.clone(),
                ),
                adapter_val: Some("codex/managed"),
                caps: &["cli.read", "cli.write", "wiki.read"],
                profile_tools: Some(vec!["cadence_writes"]),
                bad_digest: None,
                profile_id: None,
                expected_substr: "server_not_ready",
            },
            NegCase {
                label: "server_actor_service_override",
                issue_payload: base_issue(
                    json!({
                        "dedicated_updater": "agents/ap_upd",
                        "server_readiness": { "ready": true, "actor": "service/pa_1", "reasons": [] }
                    }),
                    one_art.clone(),
                ),
                adapter_val: Some("codex/managed"),
                caps: &["cli.read", "cli.write", "wiki.read"],
                profile_tools: Some(vec!["cadence_writes"]),
                bad_digest: None,
                profile_id: None,
                expected_substr: "personal_assistant_excluded",
            },
            NegCase {
                label: "claimed_profile_mismatch",
                issue_payload: base_issue(
                    json!({"updater_agent": "agents/ap_other"}),
                    one_art.clone(),
                ),
                adapter_val: Some("codex/managed"),
                caps: &["cli.read", "cli.write", "wiki.read"],
                profile_tools: Some(vec!["cadence_writes"]),
                bad_digest: None,
                profile_id: Some("ap_claimed"),
                expected_substr: "actor_mismatch",
            },
            NegCase {
                label: "corrupt_digest",
                issue_payload: base_issue(
                    json!({"dedicated_updater": "agents/ap_upd"}),
                    one_art.clone(),
                ),
                adapter_val: Some("codex/managed"),
                caps: &["cli.read", "cli.write", "wiki.read"],
                profile_tools: Some(vec!["cadence_writes"]),
                bad_digest: Some(
                    "0000000000000000000000000000000000000000000000000000000000000000",
                ),
                profile_id: Some("ap_upd"),
                expected_substr: "digest",
            },
            NegCase {
                label: "ambiguous_multiple_artifacts",
                issue_payload: base_issue(
                    json!({"dedicated_updater": "agents/ap_upd"}),
                    json!([
                        {"id": art_id, "kind": "private-app-zip", "version": 1, "name": "a.zip"},
                        {"id": format!("art-{}", "3".repeat(32)), "kind": "private-app-zip", "version": 1, "name": "b.zip"},
                    ]),
                ),
                adapter_val: Some("codex/managed"),
                caps: &["cli.read", "cli.write", "wiki.read"],
                profile_tools: Some(vec!["cadence_writes"]),
                bad_digest: None,
                profile_id: Some("ap_upd"),
                expected_substr: "multiple private-app-zip",
            },
            NegCase {
                label: "absent_claimed_profile",
                issue_payload: base_issue(
                    json!({"dedicated_updater": "agents/ap_upd"}),
                    one_art.clone(),
                ),
                adapter_val: Some("codex/managed"),
                caps: &["cli.read", "cli.write", "wiki.read"],
                profile_tools: Some(vec!["cadence_writes"]),
                bad_digest: None,
                profile_id: None,
                expected_substr: "actor_mismatch",
            },
            NegCase {
                label: "chat_default_as_dedicated_updater",
                issue_payload: base_issue(
                    json!({"dedicated_updater": "agents/ap_chat", "default_app_agent": "ap_chat"}),
                    one_art.clone(),
                ),
                adapter_val: Some("codex/managed"),
                caps: &["cli.read", "cli.write", "wiki.read"],
                profile_tools: Some(vec!["cadence_writes"]),
                bad_digest: None,
                profile_id: Some("ap_chat"),
                expected_substr: "chat_default_not_updater",
            },
            NegCase {
                label: "chat_default_as_workspace_default_updater",
                issue_payload: base_issue(
                    json!({"workspace_default_updater": "agents/ap_chat", "default_app_agent": "ap_chat"}),
                    one_art.clone(),
                ),
                adapter_val: Some("codex/managed"),
                caps: &["cli.read", "cli.write", "wiki.read"],
                profile_tools: Some(vec!["cadence_writes"]),
                bad_digest: None,
                profile_id: Some("ap_chat"),
                expected_substr: "chat_default_not_updater",
            },
        ];

        for (idx, case) in cases.into_iter().enumerate() {
            let NegCase {
                label,
                issue_payload,
                adapter_val,
                caps,
                profile_tools,
                bad_digest,
                profile_id,
                expected_substr,
            } = case;
            let state_dir = root.join(format!("state-{idx}"));
            fs::create_dir_all(&state_dir).unwrap();
            let (endpoint, _rec, handle) = spawn_mock_board(
                issue_payload,
                zip_bytes.clone(),
                bad_digest.map(str::to_string),
                review_id.clone(),
                profile_tools.map(|v| v.into_iter().map(str::to_string).collect()),
            );
            write_login(&root, &endpoint, caps);
            let target = RemoteTarget {
                org: "acme".into(),
                endpoint: endpoint.clone(),
                org_id: "org_1".into(),
                identity: Some(remote_cli::CliIdentityPin {
                    subject_id: "sub_12345".into(),
                    issuer: "https://auth.example".into(),
                }),
            };
            let msg_id = format!("msg-neg-{idx}");
            let kickoff = Kickoff {
                alias: "updater",
                issue_id: "CAD-2584",
                assignment_id: "hla_test1",
                worktree: "/tmp/wt-neg",
                message_id: &msg_id,
                claim_id: "hlc_test1",
                generation: 1,
                organization_id: "org_1",
                audience: &endpoint,
                subject_id: Some("sub_12345"),
                issuer: Some("https://auth.example"),
                profile_id,
            };
            let send_called = Arc::new(AtomicBool::new(false));
            let send_called_cb = Arc::clone(&send_called);
            let adapter_owned = adapter_val.map(str::to_string);
            let err = deliver_kickoff_with_adapter(
                &target,
                &state_dir,
                &kickoff,
                10,
                &|| adapter_owned.clone(),
                &|_| {
                    send_called_cb.store(true, Ordering::SeqCst);
                    Ok(json!({}))
                },
            )
            .unwrap_err();
            assert!(!send_called.load(Ordering::SeqCst), "{label} called send");
            assert!(
                err.to_string().contains(expected_substr),
                "{label}: unexpected error {err}"
            );
            assert!(
                crate::local_assignment::reporting::hosted_binding_target(&state_dir, &msg_id)
                    .unwrap()
                    .is_none(),
                "{label} wrote .bind"
            );
            assert!(
                !crate::local_assignment::reporting::private_app_package_path(&state_dir, &msg_id)
                    .unwrap()
                    .exists(),
                "{label} wrote .zip"
            );
            drop(handle);
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn non_private_app_issue_performs_zero_extra_rpc_or_adapter_lookup() {
        let root = std::env::temp_dir().join(format!("cad2584-del-noop-{}", uuid::Uuid::new_v4()));
        let state_dir = root.join("state");
        fs::create_dir_all(&state_dir).unwrap();
        let _env = HomeEnvGuard::enter(&root);

        let issue_payload = json!({
            "issue": { "id": "CAD-9999", "title": "Ordinary dev ticket", "frontmatter": {} },
            "artifacts": [{"name": "hosted:notes--art-999-v1.md", "size": 42}],
        });
        let (endpoint, recorded, handle) = spawn_mock_board(
            issue_payload,
            b"PK\x03\x04x".to_vec(),
            None,
            format!("par_{}", "b".repeat(32)),
            None,
        );
        write_login(&root, &endpoint, &["cli.read", "cli.write", "wiki.read"]);
        let target = RemoteTarget {
            org: "acme".into(),
            endpoint: endpoint.clone(),
            org_id: "org_1".into(),
            identity: Some(remote_cli::CliIdentityPin {
                subject_id: "sub_12345".into(),
                issuer: "https://auth.example".into(),
            }),
        };
        let kickoff = Kickoff {
            alias: "worker",
            issue_id: "CAD-9999",
            assignment_id: "hla_test9",
            worktree: "/tmp/wt-9",
            message_id: "msg-noop-9",
            claim_id: "hlc_test9",
            generation: 1,
            organization_id: "org_1",
            audience: &endpoint,
            subject_id: Some("sub_12345"),
            issuer: Some("https://auth.example"),
            profile_id: None,
        };
        let adapter_calls = AtomicUsize::new(0);
        let out = deliver_kickoff_with_adapter(
            &target,
            &state_dir,
            &kickoff,
            10,
            &|| {
                adapter_calls.fetch_add(1, Ordering::SeqCst);
                Some("codex/managed".into())
            },
            &|_| Ok(json!({"message": "msg-noop-9", "state": "queued"})),
        )
        .unwrap();
        assert_eq!(out["message"], "msg-noop-9");
        assert_eq!(adapter_calls.load(Ordering::SeqCst), 0);
        let verbs: Vec<String> = recorded
            .lock()
            .unwrap()
            .iter()
            .map(|(v, _)| v.clone())
            .collect();
        assert_eq!(verbs, vec!["issue_show", "issue_history", "enumerate"]);
        let _ = fs::remove_dir_all(&root);
        drop(handle);
    }
}
