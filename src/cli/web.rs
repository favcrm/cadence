//! CAD-1429: `cadence web` — Publish Web over the AgenticOS `/v1/web-sites`
//! contract (`packages/contracts/src/web-sites.ts`, version "1").
//!
//! Every action runs against one explicitly selected workspace. The credential
//! is the saved `hct_` grant verified by `remote_cli`; this module never reads a
//! token and never prints one. Each side effect is preceded by a durable intent
//! record, so an interrupted command resumes the original request instead of
//! inventing a new one. Publish, rollback and unpublish only create owner
//! approval actions; approval happens in a browser, never here.

use super::*;
use cadence_agent::remote_cli::{self, RemoteTarget, WebIdentity};
use cadence_agent::web_publish;
use serde::{Deserialize, Serialize};
use serde_json::Map;

const WEB_VERSION: &str = "1";
/// The per-org pending file, beside the org's credential. Holds no secrets.
const PENDING_FILE_PREFIX: &str = "web-";

#[derive(Subcommand)]
pub(crate) enum WebAction {
    /// Validate, freeze and upload a plain HTML file (published as index.html)
    /// or a built static folder to the selected workspace, then finalize a
    /// ready release. Nothing is made public. No npm, build or project script
    /// runs.
    Prepare {
        /// A `.html` file or the built output folder (for example `dist/`).
        input: PathBuf,
        /// Site slug (3-63 lowercase letters, digits or hyphens).
        #[arg(long)]
        name: String,
        /// Opt in to SPA fallback: unmatched navigation paths serve index.html.
        /// Off by default; part of the release the owner approves.
        #[arg(long)]
        spa: bool,
    },
    /// Ask the owner to approve publishing a prepared release. Prints the
    /// action; the live URL appears only after a confirmed owner approval.
    Publish {
        /// Release id returned by `prepare`.
        release: String,
    },
    /// Show a site's live state and any action this machine has in flight.
    Status {
        /// Site slug.
        slug: String,
    },
    /// List a site's releases, newest first.
    Releases {
        /// Site slug.
        slug: String,
    },
    /// Ask the owner to restore an earlier release (owner approval).
    Rollback {
        /// Site slug.
        slug: String,
        /// Release id to restore.
        release: String,
    },
    /// Ask the owner to take the live site offline (owner approval).
    Unpublish {
        /// Site slug.
        slug: String,
    },
}

/// One durable intent: the exact request this machine means to make, written
/// before its first side effect. A later invocation with the same intent
/// reuses it (same key, same body) and never re-reads state to build a new one.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct Intent {
    /// `prepare`, `publish`, `rollback` or `unpublish`.
    kind: String,
    slug: String,
    /// Upload intents: the manifest digest. Action intents: empty.
    #[serde(default)]
    digest: String,
    /// Upload intents: the SPA opt-in the original request carried.
    #[serde(default)]
    spa: bool,
    /// Action intents: the release the owner is asked to approve, if any.
    #[serde(default)]
    release_id: Option<String>,
    /// Action intents: the revision the request was made against.
    #[serde(default)]
    expected_revision: Option<i64>,
    /// The original request key. It is never regenerated for this intent.
    request_key: String,
    /// The exact JSON body sent for an action; reissued verbatim on a retry.
    #[serde(default)]
    body: Option<Value>,
    /// Ids the host returned, once a reply arrived. `None` means unknown.
    #[serde(default)]
    site_id: Option<String>,
    #[serde(default)]
    upload_id: Option<String>,
    #[serde(default)]
    action_id: Option<String>,
    /// `open` until the outcome is known; `done` once the host confirmed a
    /// terminal state; `failed` once the host refused the request.
    state: String,
}

/// The pending record for one org, bound to the identity it was written under.
#[derive(Serialize, Deserialize, Default, Debug)]
struct PendingRecord {
    /// Written on the first save; checked against the live verified identity.
    #[serde(default)]
    identity: Option<WebIdentity>,
    /// Upload and action intents, oldest first.
    #[serde(default)]
    intents: Vec<Intent>,
    /// Releases that finalized successfully, for later publish and rollback.
    #[serde(default)]
    releases: Vec<ReleaseRef>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct ReleaseRef {
    slug: String,
    site_id: String,
    release_id: String,
    manifest_digest: String,
}

/// Run one `cadence web` action against `target`, the workspace pinned by
/// `--org` through the org registry.
pub(super) fn run(action: &WebAction, target: &RemoteTarget, wake_timeout: u64) -> Result<i32> {
    match action {
        WebAction::Prepare { input, name, spa } => prepare(target, input, name, *spa, wake_timeout),
        WebAction::Publish { release } => {
            request_action(target, "publish", None, Some(release), wake_timeout)
        }
        WebAction::Rollback { slug, release } => {
            request_action(target, "rollback", Some(slug), Some(release), wake_timeout)
        }
        WebAction::Unpublish { slug } => {
            request_action(target, "unpublish", Some(slug), None, wake_timeout)
        }
        WebAction::Status { slug } => status(target, slug, wake_timeout),
        WebAction::Releases { slug } => releases(target, slug, wake_timeout),
    }
}

fn reject(message: impl Into<String>) -> Error {
    Error::rejected(message.into())
}

fn print_json(value: &Value) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|e| Error::internal(e.to_string()))?
    );
    Ok(())
}

fn pending_path(target: &RemoteTarget) -> Result<PathBuf> {
    let dir = cadence_agent::remote_auth::auth_dir(None)?;
    Ok(dir.join(format!("{PENDING_FILE_PREFIX}{}.json", target.org)))
}

/// Load the org's pending record and bind it to the verified identity. A record
/// written under a different issuer, audience or organization id fails closed:
/// the operator must reconsent rather than have another workspace's intents
/// reused here.
fn load_pending(target: &RemoteTarget, identity: &WebIdentity) -> Result<PendingRecord> {
    let path = pending_path(target)?;
    let record: PendingRecord = match std::fs::read(&path) {
        Ok(raw) => serde_json::from_slice(&raw)
            .map_err(|_| reject("the pending web record is unreadable; nothing was sent"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => PendingRecord::default(),
        Err(e) => return Err(e.into()),
    };
    if let Some(bound) = &record.identity {
        if bound != identity {
            return Err(reject(format!(
                "the pending web record for '{}' belongs to another issuer, audience or workspace; run `cadence login --web` for this workspace. Nothing was sent",
                target.org
            )));
        }
    }
    Ok(record)
}

/// Write the record atomically, owner-only, binding it to `identity`.
fn save_pending(
    target: &RemoteTarget,
    identity: &WebIdentity,
    record: &mut PendingRecord,
) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    record.identity = Some(identity.clone());
    let path = pending_path(target)?;
    let dir = path
        .parent()
        .ok_or_else(|| reject("the pending web record path is invalid"))?;
    let tmp = dir.join(format!(".{PENDING_FILE_PREFIX}{}.tmp", target.org));
    let body = serde_json::to_vec(record).map_err(|e| Error::internal(e.to_string()))?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)?;
    file.write_all(&body)?;
    file.sync_all()?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn new_request_key(prefix: &str) -> Result<String> {
    let mut random = [0u8; 12];
    getrandom::fill(&mut random).map_err(|_| reject("unable to create a request key"))?;
    let hex: String = random.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("{prefix}-{hex}"))
}

fn is_id(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|rest| {
        (8..=80).contains(&rest.len())
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

/// `cadence web prepare`. The intent (slug, digest, spa, original key) is
/// saved before the first POST, so a lost create reply resumes with the same
/// key on the next run. Success prints only after finalize is verified.
fn prepare(
    target: &RemoteTarget,
    input: &Path,
    name: &str,
    spa: bool,
    wake_timeout: u64,
) -> Result<i32> {
    web_publish::validate_slug(name)?;
    let site = web_publish::freeze(input)?;
    let identity = remote_cli::web_identity(target)?;
    let mut record = load_pending(target, &identity)?;

    // Resume the original intent for this slug+digest, or create it now.
    let open = record.intents.iter().position(|i| {
        i.kind == "prepare" && i.slug == name && i.digest == site.digest && i.state == "open"
    });
    let index = match open {
        Some(index) => {
            if record.intents[index].spa != spa {
                return Err(reject(format!(
                    "an open prepare for '{name}' with the same content was made with a different --spa setting; resolve it with the original setting. Nothing was sent"
                )));
            }
            index
        }
        None => {
            record.intents.push(Intent {
                kind: "prepare".into(),
                slug: name.to_string(),
                digest: site.digest.clone(),
                spa,
                release_id: None,
                expected_revision: None,
                request_key: new_request_key("web-prepare")?,
                body: None,
                site_id: None,
                upload_id: None,
                action_id: None,
                state: "open".into(),
            });
            // Durable before the first side effect.
            save_pending(target, &identity, &mut record)?;
            record.intents.len() - 1
        }
    };
    let request_key = record.intents[index].request_key.clone();

    // Create or resume the upload with the SAME body and key every time. The
    // host answers a repeat with the same site/upload and the paths still
    // missing, so a resumed run learns what remains from the reply itself.
    let manifest = site.manifest.clone();
    let mut body = json!({
        "version": WEB_VERSION,
        "workspaceId": target.org_id,
        "slug": name,
        "manifest": manifest,
        "manifestDigest": site.digest,
        "requestKey": request_key,
    });
    if spa {
        body["spaFallback"] = json!(true);
    }
    let created = remote_cli::web_request(
        target,
        "web.prepare",
        "POST",
        "/v1/web-sites/uploads",
        &[("content-type", "application/json".to_string())],
        Some(&serde_json::to_vec(&body).map_err(|e| Error::internal(e.to_string()))?),
        wake_timeout,
    )?;
    let created = remote_cli::web_envelope(created)?;
    let site_id = created["siteId"].as_str().unwrap_or_default().to_string();
    let upload_id = created["uploadId"].as_str().unwrap_or_default().to_string();
    if !is_id(&site_id, "site_") || !is_id(&upload_id, "upl_") {
        return Err(reject(
            "the host upload reply was not a valid site/upload id",
        ));
    }
    let missing: Vec<String> = created["missingPaths"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    record.intents[index].site_id = Some(site_id.clone());
    record.intents[index].upload_id = Some(upload_id.clone());
    save_pending(target, &identity, &mut record)?;

    // Stream every path the host still needs. `missing` is empty on a resumed
    // upload, so the host decides what remains via a fresh create reply.
    let by_path: std::collections::BTreeMap<&str, &web_publish::FrozenFile> =
        site.files.iter().map(|f| (f.path.as_str(), f)).collect();
    for path in &missing {
        let file = by_path.get(path.as_str()).ok_or_else(|| {
            reject("the host requested a path that is not in the frozen manifest")
        })?;
        let uploaded = remote_cli::web_request(
            target,
            "web.prepare",
            "PUT",
            &format!(
                "/v1/web-sites/uploads/{upload_id}/files/{}",
                web_publish::encode_artifact_path(path)
            ),
            &[
                ("x-workspace-id", target.org_id.clone()),
                ("x-content-sha256", file.sha256.clone()),
                ("content-type", file.mime.to_string()),
            ],
            Some(&file.bytes),
            wake_timeout,
        )?;
        remote_cli::web_envelope(uploaded)?;
    }

    let finalized = remote_cli::web_request(
        target,
        "web.prepare",
        "POST",
        &format!("/v1/web-sites/uploads/{upload_id}/finalize"),
        &[("x-workspace-id", target.org_id.clone())],
        None,
        wake_timeout,
    )?;
    let release = remote_cli::web_envelope(finalized)?;
    let release_id = release["releaseId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if !is_id(&release_id, "rel_") || release["state"] != "ready" {
        return Err(reject("the host finalize reply was not a ready release"));
    }
    if release["manifestDigest"].as_str() != Some(site.digest.as_str()) {
        return Err(reject("the host finalized a different manifest digest"));
    }
    // Only a verified finalize reaches this point: record the release, close
    // the intent, then report success.
    record.intents[index].state = "done".into();
    record.releases.push(ReleaseRef {
        slug: name.to_string(),
        site_id: site_id.clone(),
        release_id: release_id.clone(),
        manifest_digest: site.digest.clone(),
    });
    save_pending(target, &identity, &mut record)?;
    print_json(&json!({
        "phase": "ready",
        "org": target.org,
        "site": name,
        "siteId": site_id,
        "releaseId": release_id,
        "manifestDigest": site.digest,
        "files": site.files.len(),
        "totalBytes": site.total_bytes,
        "spaFallback": spa,
        "liveUrl": Value::Null,
        "next": format!("cadence web publish {release_id} --org {}", target.org),
    }))?;
    Ok(0)
}

/// Publish, rollback and unpublish. A matching open action intent is resumed
/// with its original key and body; the revision is read only when a new intent
/// is made, never to replace an unresolved one.
fn request_action(
    target: &RemoteTarget,
    operation: &str,
    slug: Option<&str>,
    release: Option<&str>,
    wake_timeout: u64,
) -> Result<i32> {
    let identity = remote_cli::web_identity(target)?;
    let mut record = load_pending(target, &identity)?;
    let (site_slug, site_id, release_id) =
        resolve_target(&record, target, operation, slug, release)?;

    // An open intent for the same operation and release is reused verbatim.
    let existing = record.intents.iter().position(|i| {
        i.kind == operation
            && i.slug == site_slug
            && i.release_id == release_id
            && i.state == "open"
    });
    let index = match existing {
        Some(index) => index,
        None => {
            let revision = fetch_revision(target, &site_id, wake_timeout)?;
            let mut body = Map::new();
            body.insert("version".into(), json!(WEB_VERSION));
            body.insert("workspaceId".into(), json!(target.org_id));
            body.insert("operation".into(), json!(operation));
            if let Some(rel) = &release_id {
                body.insert("releaseId".into(), json!(rel));
            }
            body.insert("expectedRevision".into(), json!(revision));
            let request_key = new_request_key(&format!("web-{operation}"))?;
            body.insert("requestKey".into(), json!(request_key));
            record.intents.push(Intent {
                kind: operation.to_string(),
                slug: site_slug.clone(),
                digest: String::new(),
                spa: false,
                release_id: release_id.clone(),
                expected_revision: Some(revision),
                request_key,
                body: Some(Value::Object(body)),
                site_id: Some(site_id.clone()),
                upload_id: None,
                action_id: None,
                state: "open".into(),
            });
            save_pending(target, &identity, &mut record)?;
            record.intents.len() - 1
        }
    };
    let intent = record.intents[index].clone();
    let payload = serde_json::to_vec(
        intent
            .body
            .as_ref()
            .ok_or_else(|| reject("the pending action has no recorded body"))?,
    )
    .map_err(|e| Error::internal(e.to_string()))?;
    let action = create_action_reconciled(
        target,
        &site_id,
        &payload,
        &intent.request_key,
        wake_timeout,
    )?;
    let action_id = action["actionId"].as_str().unwrap_or_default().to_string();
    if is_id(&action_id, "act_") {
        record.intents[index].action_id = Some(action_id.clone());
    }
    let state = action["state"].as_str().unwrap_or("unknown").to_string();
    if matches!(
        state.as_str(),
        "completed" | "declined" | "failed" | "expired"
    ) {
        record.intents[index].state = if state == "completed" {
            "done"
        } else {
            "failed"
        }
        .into();
    }
    save_pending(target, &identity, &mut record)?;
    report_action(target, &intent.request_key, &action)
}

/// Resolve the release/site a publish, rollback or unpublish names, from the
/// prepared releases of this org. A foreign or unknown release is refused
/// before any request is built.
fn resolve_target(
    record: &PendingRecord,
    target: &RemoteTarget,
    operation: &str,
    slug: Option<&str>,
    release: Option<&str>,
) -> Result<(String, String, Option<String>)> {
    match (operation, slug, release) {
        ("publish", None, Some(rel)) => {
            let found = record
                .releases
                .iter()
                .find(|r| r.release_id == rel)
                .ok_or_else(|| {
                    reject(format!(
                        "release '{rel}' is not a prepared release for org '{}'; nothing was sent",
                        target.org
                    ))
                })?;
            Ok((
                found.slug.clone(),
                found.site_id.clone(),
                Some(rel.to_string()),
            ))
        }
        ("rollback", Some(slug), Some(rel)) => {
            let found = record
                .releases
                .iter()
                .find(|r| r.slug == slug && r.release_id == rel)
                .ok_or_else(|| {
                    reject(format!(
                        "release '{rel}' is not a prepared release of '{slug}' for org '{}'; nothing was sent",
                        target.org
                    ))
                })?;
            Ok((
                found.slug.clone(),
                found.site_id.clone(),
                Some(rel.to_string()),
            ))
        }
        ("unpublish", Some(slug), None) => {
            let found = record
                .releases
                .iter()
                .rev()
                .find(|r| r.slug == slug)
                .ok_or_else(|| {
                    reject(format!(
                        "no prepared site '{slug}' for org '{}'; nothing was sent",
                        target.org
                    ))
                })?;
            Ok((found.slug.clone(), found.site_id.clone(), None))
        }
        _ => Err(reject("web action arguments are invalid")),
    }
}

/// The site's current revision, read from the scoped site record.
fn fetch_revision(target: &RemoteTarget, site_id: &str, wake_timeout: u64) -> Result<i64> {
    let response = remote_cli::web_request(
        target,
        "web.read",
        "GET",
        &format!("/v1/web-sites/sites/{site_id}"),
        &[("x-workspace-id", target.org_id.clone())],
        None,
        wake_timeout,
    )?;
    let site = remote_cli::web_envelope(response)?;
    site["revision"]
        .as_i64()
        .ok_or_else(|| reject("the host site reply had no revision"))
}

/// Create one owner-approval action. An unknown outcome (a transport failure or
/// a server error after the request may have landed) is reconciled by sending
/// the SAME body and key again, never a new key. If it is still unknown, the
/// intent stays open and is reported by its key.
fn create_action_reconciled(
    target: &RemoteTarget,
    site_id: &str,
    payload: &[u8],
    request_key: &str,
    wake_timeout: u64,
) -> Result<Value> {
    let path = format!("/v1/web-sites/sites/{site_id}/actions");
    let send = || {
        remote_cli::web_request(
            target,
            "web.prepare",
            "POST",
            &path,
            &[("content-type", "application/json".to_string())],
            Some(payload),
            wake_timeout,
        )
    };
    if let Ok(response) = send() {
        if response.status < 500 {
            return remote_cli::web_envelope(response);
        }
    }
    match send() {
        Ok(response) if response.status < 500 => remote_cli::web_envelope(response),
        _ => Err(Error::busy_coded(
            "unknown",
            format!(
                "the action outcome is unknown; `cadence web status` will show it for request key {request_key}. No new key was sent"
            ),
        )),
    }
}

/// Print an action without implying success. `approvalUrl` is null on the host
/// until the owner confirmation page exists, so the result is reported as
/// pending; `liveUrl` appears only on a confirmed `completed` state.
fn report_action(target: &RemoteTarget, request_key: &str, action: &Value) -> Result<i32> {
    let state = action["state"].as_str().unwrap_or("unknown");
    let live = if state == "completed" {
        action["liveUrl"].clone()
    } else {
        Value::Null
    };
    let approval = match action["approvalUrl"].as_str() {
        Some(url) => json!({"url": url, "expiresAt": action["approvalExpiresAt"]}),
        None => json!({
            "status": "owner_confirmation_unavailable",
            "reason": "the owner confirmation page is not available yet; no approval was made",
        }),
    };
    print_json(&json!({
        "org": target.org,
        "actionId": action["actionId"],
        "operation": action["operation"],
        "state": state,
        "requestKey": request_key,
        "liveUrl": live,
        "approval": approval,
    }))?;
    match state {
        "completed" | "awaiting_approval" | "approved" | "promoting" => Ok(0),
        _ => Err(reject(format!("web action ended in state '{state}'"))),
    }
}

/// Site status by slug, plus this machine's open intents for the slug. Each
/// known action id is read back from the host; an intent with no id yet is
/// shown as unknown, never given an invented status.
fn status(target: &RemoteTarget, slug: &str, wake_timeout: u64) -> Result<i32> {
    let identity = remote_cli::web_identity(target)?;
    let record = load_pending(target, &identity)?;
    let response = remote_cli::web_request(
        target,
        "web.read",
        "GET",
        &format!("/v1/web-sites/sites/by-slug/{slug}"),
        &[("x-workspace-id", target.org_id.clone())],
        None,
        wake_timeout,
    )?;
    let site = remote_cli::web_envelope(response)?;
    let mut pending = Vec::new();
    for intent in record
        .intents
        .iter()
        .filter(|i| i.slug == slug && i.state == "open" && i.kind != "prepare")
    {
        let read_back = match &intent.action_id {
            Some(action_id) if is_id(action_id, "act_") => {
                let response = remote_cli::web_request(
                    target,
                    "web.read",
                    "GET",
                    &format!("/v1/web-sites/actions/{action_id}"),
                    &[("x-workspace-id", target.org_id.clone())],
                    None,
                    wake_timeout,
                )?;
                json!({"actionId": action_id, "status": remote_cli::web_envelope(response)?})
            }
            _ => json!({"actionId": Value::Null, "status": "unknown",
                "reason": "the create reply was lost; the action id is not known"}),
        };
        pending.push(json!({
            "operation": intent.kind,
            "requestKey": intent.request_key,
            "releaseId": intent.release_id,
            "expectedRevision": intent.expected_revision,
            "action": read_back,
        }));
    }
    print_json(&json!({"org": target.org, "site": site, "pendingActions": pending}))?;
    Ok(0)
}

fn releases(target: &RemoteTarget, slug: &str, wake_timeout: u64) -> Result<i32> {
    let found = remote_cli::web_request(
        target,
        "web.read",
        "GET",
        &format!("/v1/web-sites/sites/by-slug/{slug}"),
        &[("x-workspace-id", target.org_id.clone())],
        None,
        wake_timeout,
    )?;
    let site = remote_cli::web_envelope(found)?;
    let site_id = site["siteId"].as_str().unwrap_or_default().to_string();
    let response = remote_cli::web_request(
        target,
        "web.read",
        "GET",
        &format!("/v1/web-sites/sites/{site_id}/releases"),
        &[("x-workspace-id", target.org_id.clone())],
        None,
        wake_timeout,
    )?;
    let listing = remote_cli::web_envelope(response)?;
    print_json(&json!({"org": target.org, "site": slug, "releases": listing["releases"]}))?;
    Ok(0)
}
