//! CAD-1429/CAD-1642: the Publish Web workflow over the AgenticOS
//! `/v1/web-sites` contract (`packages/contracts/src/web-sites.ts`, version
//! "1"). The daemon `agenticos-web` adapter calls the per-action functions
//! below from its account-bound entry; the acceptance harness uses [`run`].
//! The CLI is a thin staging client in `cli/web.rs`.
//!
//! Every action runs against one explicitly selected workspace. The credential
//! is the saved `hct_` grant verified by `remote_cli`; this module never reads a
//! token and never prints one. Each side effect is preceded by a durable intent
//! record, so an interrupted command resumes the original request instead of
//! inventing a new one. Publish, rollback and unpublish only create owner
//! approval actions; approval happens in a browser, never here.

use crate::error::{Error, Result};
use crate::remote_cli::{self, RemoteTarget, WebIdentity};
use crate::web_publish;
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::PathBuf;

const WEB_VERSION: &str = "1";
/// The per-org pending file, beside the org's credential. Holds no secrets.
const PENDING_FILE_PREFIX: &str = "web-";

#[derive(Subcommand)]
pub enum WebAction {
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
    /// Read a site's form submissions, newest first.
    Submissions {
        /// Site slug.
        slug: String,
        /// Only submissions of this form key.
        #[arg(long)]
        form: Option<String>,
        /// Submissions to return, 1 to 100.
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Opaque cursor from the previous page's nextCursor.
        #[arg(long)]
        cursor: Option<String>,
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

/// Run one action and return its result without printing. The `agenticos-web`
/// adapter calls the per-action functions from `execute_for_account`; the
/// acceptance harness uses [`run`] for its print/exit wrapper.
pub fn execute(action: &WebAction, target: &RemoteTarget, wake_timeout: u64) -> Result<Value> {
    match action {
        WebAction::Prepare { input, name, spa } => {
            let site = web_publish::freeze(input)?;
            prepare_frozen(target, site, name, *spa, wake_timeout)
        }
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
        WebAction::Submissions {
            slug,
            form,
            limit,
            cursor,
        } => submissions(
            target,
            slug,
            form.as_deref(),
            *limit,
            cursor.as_deref(),
            wake_timeout,
        ),
    }
}

/// The harness entry: execute, print the result, and map an action's terminal
/// state to the exit code the pre-cutover command returned.
pub fn run(action: &WebAction, target: &RemoteTarget, wake_timeout: u64) -> Result<i32> {
    let value = execute(action, target, wake_timeout)?;
    print_json(&value)?;
    if matches!(
        action,
        WebAction::Publish { .. } | WebAction::Rollback { .. } | WebAction::Unpublish { .. }
    ) {
        return match value["state"].as_str().unwrap_or("unknown") {
            "completed" | "awaiting_approval" | "approved" | "promoting" => Ok(0),
            state => Err(reject(format!("web action ended in state '{state}'"))),
        };
    }
    Ok(0)
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
    let dir = crate::remote_auth::auth_dir(None)?;
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
pub fn prepare_frozen(
    target: &RemoteTarget,
    site: web_publish::FrozenSite,
    name: &str,
    spa: bool,
    wake_timeout: u64,
) -> Result<Value> {
    web_publish::validate_slug(name)?;
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
    Ok(json!({
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
    }))
}

/// Publish, rollback and unpublish. A matching open action intent is resumed
/// with its original key and body; the revision is read only when a new intent
/// is made, never to replace an unresolved one.
pub fn request_action(
    target: &RemoteTarget,
    operation: &str,
    slug: Option<&str>,
    release: Option<&str>,
    wake_timeout: u64,
) -> Result<Value> {
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
    let existing = if let Some(index) = existing {
        if let Some(action_id) = record.intents[index].action_id.as_deref() {
            let action =
                read_known_action(target, &record.intents[index], action_id, wake_timeout)?;
            if let Some(state) = terminal_action_state(action["state"].as_str()) {
                record.intents[index].state = state.into();
                save_pending(target, &identity, &mut record)?;
                None
            } else {
                Some(index)
            }
        } else {
            Some(index)
        }
    } else {
        None
    };
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
    let action = match create_action_reconciled(
        target,
        &site_id,
        &payload,
        &intent.request_key,
        wake_timeout,
    ) {
        Ok(action) => action,
        Err(ActionCreateFailure::DefiniteRefusal(error)) => {
            record.intents[index].state = "failed".into();
            save_pending(target, &identity, &mut record)?;
            return Err(error);
        }
        Err(ActionCreateFailure::Unknown(error)) => return Err(error),
    };
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
enum ActionCreateFailure {
    DefiniteRefusal(Error),
    Unknown(Error),
}

fn definite_action_refusal(response: &remote_cli::WebResponse) -> bool {
    (400..500).contains(&response.status)
        && !matches!(response.status, 401 | 403)
        && response.body.as_ref().is_some_and(|body| {
            body["ok"] == Value::Bool(false)
                && body["error"]["code"]
                    .as_str()
                    .is_some_and(|code| !code.is_empty())
        })
}

fn create_action_reconciled(
    target: &RemoteTarget,
    site_id: &str,
    payload: &[u8],
    request_key: &str,
    wake_timeout: u64,
) -> std::result::Result<Value, ActionCreateFailure> {
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
    let mut failure = None;
    for _ in 0..2 {
        match send() {
            Ok(response) if definite_action_refusal(&response) => {
                return Err(ActionCreateFailure::DefiniteRefusal(
                    remote_cli::web_envelope(response)
                        .expect_err("validated refusal envelope must be an error"),
                ));
            }
            Ok(response) if response.status < 500 => match remote_cli::web_envelope(response) {
                Ok(action) => return Ok(action),
                Err(error) => failure = Some(error),
            },
            Ok(_) | Err(_) => {}
        }
    }
    Err(ActionCreateFailure::Unknown(failure.unwrap_or_else(|| {
        Error::busy_coded(
            "unknown",
            format!(
                "the action outcome is unknown; `cadence web status` will show it for request key {request_key}. No new key was sent"
            ),
        )
    })))
}

/// Print an action without implying success. `approvalUrl` is null on the host
/// until the owner confirmation page exists, so the result is reported as
/// pending; `liveUrl` appears only on a confirmed `completed` state.
fn terminal_action_state(state: Option<&str>) -> Option<&'static str> {
    match state {
        Some("completed") => Some("done"),
        Some("declined" | "failed" | "expired") => Some("failed"),
        _ => None,
    }
}

/// Read back an action only through its known id and require the complete
/// immutable intent binding to match before trusting its state.
fn read_known_action(
    target: &RemoteTarget,
    intent: &Intent,
    action_id: &str,
    wake_timeout: u64,
) -> Result<Value> {
    if !is_id(action_id, "act_") {
        return Err(reject("the pending action id is invalid; nothing was sent"));
    }
    let site_id = intent
        .site_id
        .as_deref()
        .ok_or_else(|| reject("the pending action has no site binding; nothing was sent"))?;
    let response = remote_cli::web_request(
        target,
        "web.read",
        "GET",
        &format!("/v1/web-sites/actions/{action_id}"),
        &[("x-workspace-id", target.org_id.clone())],
        None,
        wake_timeout,
    )?;
    if response.status != 200 {
        return Err(Error::busy_coded(
            "unknown",
            "the known action could not be safely reconciled; its intent remains open",
        ));
    }
    let action = remote_cli::web_envelope(response)?;
    if action["actionId"].as_str() != Some(action_id)
        || action["siteId"].as_str() != Some(site_id)
        || action["operation"].as_str() != Some(intent.kind.as_str())
        || action["releaseId"].as_str().map(str::to_owned) != intent.release_id
        || action["expectedRevision"].as_i64() != intent.expected_revision
        || !matches!(
            action["state"].as_str(),
            Some(
                "awaiting_approval"
                    | "approved"
                    | "promoting"
                    | "completed"
                    | "declined"
                    | "failed"
                    | "expired"
            )
        )
    {
        return Err(reject(
            "the host action does not match its saved intent; nothing was sent",
        ));
    }
    Ok(action)
}

fn report_action(target: &RemoteTarget, request_key: &str, action: &Value) -> Result<Value> {
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
    Ok(json!({
        "org": target.org,
        "actionId": action["actionId"],
        "operation": action["operation"],
        "state": state,
        "requestKey": request_key,
        "liveUrl": live,
        "approval": approval,
    }))
}

/// Site status by slug, plus this machine's open intents for the slug. Each
/// known action id is read back from the host; an intent with no id yet is
/// shown as unknown, never given an invented status.
pub fn status(target: &RemoteTarget, slug: &str, wake_timeout: u64) -> Result<Value> {
    let identity = remote_cli::web_identity(target)?;
    let mut record = load_pending(target, &identity)?;
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
    for index in 0..record.intents.len() {
        let intent = record.intents[index].clone();
        if intent.slug != slug || intent.state != "open" || intent.kind == "prepare" {
            continue;
        }
        let read_back = match intent.action_id.as_deref() {
            Some(action_id) => {
                let action = read_known_action(target, &intent, action_id, wake_timeout)?;
                if let Some(state) = terminal_action_state(action["state"].as_str()) {
                    record.intents[index].state = state.into();
                    save_pending(target, &identity, &mut record)?;
                }
                json!({"actionId": action_id, "status": action})
            }
            None => json!({"actionId": Value::Null, "status": "unknown",
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
    Ok(json!({"org": target.org, "site": site, "pendingActions": pending}))
}

/// A site's form submissions, newest first. `form` filters one form key and
/// `cursor` is the opaque `nextCursor` returned by the previous page; a
/// malformed cursor is refused locally rather than sent.
pub fn submissions(
    target: &RemoteTarget,
    slug: &str,
    form: Option<&str>,
    limit: u32,
    cursor: Option<&str>,
    wake_timeout: u64,
) -> Result<Value> {
    validate_submission_query(form, limit, cursor)?;
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
    let mut path = format!("/v1/web-sites/sites/{site_id}/submissions?limit={limit}");
    if let Some(form) = form {
        path.push_str("&form=");
        path.push_str(form);
    }
    if let Some(cursor) = cursor {
        path.push_str("&cursor=");
        path.push_str(cursor);
    }
    let response = remote_cli::web_request(
        target,
        "web.read",
        "GET",
        &path,
        &[("x-workspace-id", target.org_id.clone())],
        None,
        wake_timeout,
    )?;
    let listing = remote_cli::web_envelope(response)?;
    Ok(json!({
        "org": target.org,
        "site": slug,
        "submissions": listing["submissions"],
        "nextCursor": listing["nextCursor"],
    }))
}

pub fn releases(target: &RemoteTarget, slug: &str, wake_timeout: u64) -> Result<Value> {
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
    Ok(json!({"org": target.org, "site": slug, "releases": listing["releases"]}))
}

/// Local pre-flight for `submissions`. The server owns the query grammar; this
/// refuses the obviously invalid before any request is built, using the same
/// form-key grammar as the published endpoint (`^[a-z0-9][a-z0-9-]{0,39}$`).
pub fn validate_submission_query(
    form: Option<&str>,
    limit: u32,
    cursor: Option<&str>,
) -> Result<()> {
    if !(1..=100).contains(&limit) {
        return Err(Error::rejected("--limit must be between 1 and 100"));
    }
    if let Some(form) = form {
        if !is_form_key(form) {
            return Err(Error::rejected(
                "--form must match ^[a-z0-9][a-z0-9-]{0,39}$",
            ));
        }
    }
    if let Some(cursor) = cursor {
        if !is_form_cursor(cursor) {
            return Err(Error::rejected(
                "--cursor must be a nextCursor value from a previous page",
            ));
        }
    }
    Ok(())
}

/// The published form-key grammar: one lowercase letter or digit, then up to 39
/// more lowercase letters, digits or hyphens.
pub fn is_form_key(value: &str) -> bool {
    matches!(value.as_bytes().first(), Some(b'a'..=b'z' | b'0'..=b'9'))
        && value.len() <= 40
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The published submissions cursor: `<created_at ms>:<wsub id>`.
pub fn is_form_cursor(value: &str) -> bool {
    let Some((millis, id)) = value.split_once(':') else {
        return false;
    };
    let Some(suffix) = id.strip_prefix("wsub_") else {
        return false;
    };
    (1..=16).contains(&millis.len())
        && millis.bytes().all(|b| b.is_ascii_digit())
        && (8..=80).contains(&suffix.len())
        && suffix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(test)]
mod submission_query_tests {
    use super::validate_submission_query;

    #[test]
    fn form_keys_follow_the_published_grammar() {
        let longest = "x".repeat(40);
        for good in ["contact", "a", "contact-", "a1-2", longest.as_str()] {
            assert!(
                validate_submission_query(Some(good), 50, None).is_ok(),
                "{good} must be accepted"
            );
        }
        let too_long = "x".repeat(41);
        for bad in ["", "-lead", "Contact", "con tact", too_long.as_str()] {
            assert!(
                validate_submission_query(Some(bad), 50, None).is_err(),
                "{bad:?} must be refused"
            );
        }
    }

    #[test]
    fn limit_and_cursor_are_refused_before_any_request() {
        assert!(validate_submission_query(None, 1, None).is_ok());
        assert!(validate_submission_query(None, 100, None).is_ok());
        assert!(validate_submission_query(None, 0, None).is_err());
        assert!(validate_submission_query(None, 101, None).is_err());
        assert!(validate_submission_query(None, 50, Some("1730000000000:wsub_abcdefgh")).is_ok());
        for bad in [
            "1730000000000:wsub_abc",
            "1730000000000:abc",
            "x:wsub_abcdefgh",
            "1730000000000:wsub_abcdefgh&limit=9",
            "a b",
            "",
        ] {
            assert!(
                validate_submission_query(None, 50, Some(bad)).is_err(),
                "{bad:?} must be refused"
            );
        }
    }
}
