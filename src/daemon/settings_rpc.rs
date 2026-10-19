//! CAD-1586: `agent_settings_command` — the daemon end of
//! `cadence settings agents` — and `cli_settings_command`, its hosted
//! peer reached through the `/api/cli/settings_agents` relay.
//!
//! Every operation converges on the one settings authority: mutations
//! patch the presented snapshot config and run through
//! [`crate::agent_settings::save`] — the same CAS + reference + live-use
//! transaction the UI's `agent_settings_set` uses; `profile.sync` /
//! `profile.preview` delegate to [`crate::agent_settings::sync`] /
//! [`crate::agent_settings::preview`]. `expected_revision` is the
//! snapshot token; a stale one is a `conflict` before anything runs.
//!
//! Authority is the connection, never a field:
//!
//! - proven operator — every operation;
//! - a settings grant-holder — exactly the bound operation and payload.
//!   A designated agent requests a grant (`grant.request`), the operator
//!   approves the exact stored payload (`grant.allow`), and the agent
//!   reruns it with `grant_id` inside the payload. The grant binds the
//!   operation, the canonical payload bytes (a `--file` replaced after
//!   approval hashes differently and is refused before mutation), the
//!   expected revision, the requester's identity (alias + registration
//!   generation + peer uid), the org, a 15-minute expiry and a single
//!   use. Ordinary workers, member assistants, the master and unproven
//!   callers hold nothing: `grant.request` and `grant.list` are the only
//!   operations an agent connection may reach ungranted — a request is
//!   never authority.
//! - An approved-command child ([`Shared::grant_exec_matches`]) may read,
//!   but runs no mutation or grant verb: an approved argv is path-bound,
//!   not byte-bound.
//! - A remote caller arrives as `cli_envelope` (the container's proven
//!   relay), re-verified here against this daemon's board identity — a
//!   verified human owner is operator-class for this org's settings;
//!   `agents/<h>` principals need a hosted grant bound to their actor
//!   spelling and this org; service actors and members are refused. A
//!   local (pid-bound) grant never satisfies a remote subject.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::*;

/// The state-dir document that outlives master being down.
const GRANTS_FILE: &str = "settings-grants.json";
/// How long a request stays decidable and a grant stays usable.
const GRANT_TTL_SECS: i64 = 15 * 60;
/// A grant org value for this daemon's own (local) settings.
const LOCAL_ORG: &str = "local";

/// Operations that are read-only (no mutation, no export write).
/// `profile.preview` is read-only but revision-bound; `catalog.refresh`
/// is classed write — it spawns the provider catalog fetch.
pub(super) const READ_OPS: &[&str] = &[
    "snapshot",
    "history",
    "harness.list",
    "harness.show",
    "provider.list",
    "provider.show",
    "model.list",
    "model.show",
    "role.list",
    "profile.list",
    "profile.show",
    "pin.list",
    "profile.preview",
];

/// Mutating, revision-bound operations (CAS on expected_revision).
pub(super) const WRITE_OPS: &[&str] = &[
    "apply",
    "harness.put",
    "harness.remove",
    "provider.put",
    "provider.remove",
    "model.put",
    "model.rename",
    "model.remove",
    "model.scope",
    "catalog.refresh",
    "catalog.import",
    "role.set",
    "role.clear",
    "profile.put",
    "profile.rename",
    "profile.remove",
    "profile.sync",
    "pin.put",
    "pin.remove",
];

/// Operator-only grant decisions — never agent-callable, never
/// grant_exec-callable.
pub(super) const GRANT_DECISION_OPS: &[&str] = &["grant.allow", "grant.reject", "grant.revoke"];
/// Agent-callable grant verbs — a petition and self-scoped listing.
pub(super) const GRANT_ASK_OPS: &[&str] = &["grant.request", "grant.list"];

pub(super) fn is_read_op(op: &str) -> bool {
    READ_OPS.contains(&op)
}
pub(super) fn is_write_op(op: &str) -> bool {
    WRITE_OPS.contains(&op)
}
pub(super) fn known_op(op: &str) -> bool {
    is_read_op(op)
        || is_write_op(op)
        || GRANT_DECISION_OPS.contains(&op)
        || GRANT_ASK_OPS.contains(&op)
        || op == "master.status"
        || op == "master.resume"
}
/// The `settings.write` envelope scope a remote op needs — mutations,
/// side-effecting fetches and grant decisions. Everything else is
/// `settings.read`.
pub(super) fn op_needs_write(op: &str) -> bool {
    is_write_op(op)
        || GRANT_DECISION_OPS.contains(&op)
        || op == "grant.request"
        || op == "master.resume"
}
/// Does `op` touch the settings document CAS (mutate or revision-bound
/// read)? `catalog.refresh` is a write-class side effect but does not
/// CAS the document — it takes no expected_revision.
pub(super) fn revision_bound(op: &str) -> bool {
    (is_write_op(op) && op != "catalog.refresh") || op == "profile.preview"
}
fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------- grant store (state_dir/settings-grants.json) ----------

/// Who a local request/grant binds: the connection-derived agent plus
/// its registration generation and the socket peer's kernel uid. Any of
/// the three changing (re-register, reopen, another uid) ends it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct AgentSubject {
    alias: String,
    generation: String,
    uid: u32,
}

/// Who a grant was minted for. `kind` is `agent` (a connection-derived
/// local subject) or `cli-agent` (a hosted `agents/<h>` principal bound
/// to an org) — a local pid-bound grant can never satisfy a remote
/// subject because the kinds differ.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind")]
enum GrantSubject {
    #[serde(rename = "agent")]
    Agent {
        alias: String,
        generation: String,
        uid: u32,
    },
    #[serde(rename = "cli-agent")]
    CliAgent { actor: String, sub: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SettingsRequest {
    id: String,
    /// The derived requester — connection identity or a re-verified
    /// hosted principal; recorded, never supplied by the request.
    subject: GrantSubject,
    /// The settings org the request was filed under — "local" for a
    /// socket caller, the verified company for a hosted principal.
    org: String,
    operation: String,
    /// The observed revision the requester bound (CAS ops must carry
    /// one; reads and refresh leave it absent).
    expected_revision: Option<String>,
    /// The exact payload the requester wants to run — the daemon
    /// digests it; the requester never supplies the digest.
    payload: Value,
    /// sha256 of canonical(payload), computed by the daemon — the
    payload_sha256: String,
    reason: String,
    created: i64,
    expires_at: i64,
    /// pending | allowed | rejected | expired
    status: String,
    decision_at: Option<i64>,
    grant_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SettingsGrant {
    id: String,
    request_id: String,
    subject: GrantSubject,
    operation: String,
    expected_revision: Option<String>,
    payload: Value,
    payload_sha256: String,
    org: String,
    issued_at: i64,
    expires_at: i64,
    revoked: bool,
    used_at: Option<i64>,
    /// How the grant was minted — "operator" or a verified remote
    /// provenance label; recorded, never a request field.
    issuer: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct GrantDoc {
    #[serde(default)]
    requests: Vec<SettingsRequest>,
    #[serde(default)]
    grants: Vec<SettingsGrant>,
}

/// Per-state-dir lock — the load/modify/save critical section.
fn grants_lock(state_dir: &Path) -> Arc<Mutex<()>> {
    static LOCKS: Mutex<Option<HashMap<String, Arc<Mutex<()>>>>> = Mutex::new(None);
    let key = state_dir.display().to_string();
    let mut map = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    let map = map.get_or_insert_with(HashMap::new);
    map.entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

fn grants_path(state_dir: &Path) -> std::path::PathBuf {
    state_dir.join(GRANTS_FILE)
}

fn grants_load(state_dir: &Path) -> Result<GrantDoc> {
    let path = grants_path(state_dir);
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(GrantDoc::default()),
        Err(e) => return Err(Error::rejected(format!("settings grants unreadable: {e}"))),
    };
    serde_json::from_str(&text)
        .map_err(|e| Error::rejected(format!("settings grants file is not valid JSON: {e}")))
}

fn grants_save(state_dir: &Path, doc: &GrantDoc) -> Result<()> {
    let path = grants_path(state_dir);
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(doc)
        .map_err(|e| Error::rejected(format!("settings grants encode failed: {e}")))?;
    fs::write(&tmp, &text)
        .map_err(|e| Error::rejected(format!("settings grants write failed: {e}")))?;
    fs::rename(&tmp, &path)
        .map_err(|e| Error::rejected(format!("settings grants commit failed: {e}")))
}

fn payload_digest(payload: &Value) -> String {
    let hash = Sha256::digest(crate::agent_settings::canonical(payload).as_bytes());
    hash.iter().map(|b| format!("{b:02x}")).collect()
}
fn request_json(r: &SettingsRequest) -> Value {
    json!({
        "id": r.id,
        "subject": r.subject,
        "org": r.org,
        "operation": r.operation,
        "expected_revision": r.expected_revision,
        "payload": r.payload,
        "payload_sha256": r.payload_sha256,
        "reason": r.reason,
        "created": r.created,
        "expires_at": r.expires_at,
        "status": r.status,
        "decision_at": r.decision_at,
        "grant_id": r.grant_id,
    })
}

fn grant_json(g: &SettingsGrant) -> Value {
    json!({
        "id": g.id,
        "request_id": g.request_id,
        "subject": g.subject,
        "operation": g.operation,
        "expected_revision": g.expected_revision,
        "payload": g.payload,
        "payload_sha256": g.payload_sha256,
        "org": g.org,
        "issued_at": g.issued_at,
        "expires_at": g.expires_at,
        "revoked": g.revoked,
        "used_at": g.used_at,
        "issuer": g.issuer,
    })
}

// ---------- caller classification ----------

/// The settings authority a connection carries, derived — never read
/// from the request.
pub(super) enum SettingsCaller {
    /// Proven operator shell.
    Operator,
    /// A daemon-spawned approved-command child (`CADENCE_GRANT_TOKEN`
    /// bound pid+argv): reads only; mutations need a settings grant the
    /// approved argv can never mint.
    GrantExec,
    /// A connection-derived agent (slot/enrollment identity).
    Agent(AgentSubject),
    /// A re-verified hosted human owner — operator-class for this org's
    /// settings; grant issuances are recorded under its provenance.
    RemoteOwner,
    /// A re-verified hosted `agents/<h>` principal — petition verbs and
    /// grant-bound operations only.
    RemoteAgent {
        actor: String,
        sub: String,
        org: String,
    },
    /// No agent identity and no operator proof.
    Unproven(String),
}

fn agent_registration_generation(agent: &crate::store::Agent) -> String {
    super::identity::owner_generation(agent)
        .unwrap_or_else(|| format!("{:016x}:no-endpoint", agent.created.to_bits()))
}

impl Shared {
    /// Classify the socket caller for settings verbs. Order matters:
    /// an approved-command child outranks the seam (its token is
    /// production authority); the seam answers before slot derivation
    /// so a pane run and a CI run decide identically; then the real
    /// slot/enrollment identity; then operator proof.
    fn settings_caller(&self, peer_pid: u32) -> Result<SettingsCaller> {
        if self.grant_exec_matches(peer_pid) {
            return Ok(SettingsCaller::GrantExec);
        }
        if let Some(asserted) = crate::test_seam::asserted() {
            return Ok(match asserted {
                crate::test_seam::Asserted::Operator => SettingsCaller::Operator,
                crate::test_seam::Asserted::Agent(alias) => {
                    match self.settings_agent_subject(&alias, peer_pid)? {
                        Some(s) => SettingsCaller::Agent(s),
                        None => SettingsCaller::Unproven(format!(
                            "test-seam agent '{alias}' has no registered row"
                        )),
                    }
                }
                crate::test_seam::Asserted::Unproven => SettingsCaller::Unproven(
                    "caller carries a test-seam 'unproven' assertion".to_string(),
                ),
            });
        }
        if self.agent_uid.is_some() {
            // In a split-uid daemon only the operator uid may carry
            // operator-class settings authority; the agent uid must
            // prove an alias.
            let uid = super::serve::frame_peer_uid(self.agent_uid)?;
            if let Some(agent_uid) = self.agent_uid {
                if uid == agent_uid {
                    self.revalidate_enrollments()?;
                    let lane = self
                        .slot_identity(peer_pid)?
                        .map(|who| who.lane().to_string());
                    return Ok(match lane {
                        Some(alias) if !alias.is_empty() => {
                            match self.settings_agent_subject(&alias, peer_pid)? {
                                Some(s) => SettingsCaller::Agent(s),
                                None => SettingsCaller::Unproven(format!(
                                    "pane agent '{alias}' has no registered row"
                                )),
                            }
                        }
                        _ => SettingsCaller::Unproven(format!(
                            "socket peer uid {uid} is agent-family but has no proved alias"
                        )),
                    });
                }
            }
        }
        self.revalidate_enrollments()?;
        if let Some(who) = self.slot_identity(peer_pid)? {
            let alias = who.lane();
            if alias.is_empty() {
                return Ok(SettingsCaller::Unproven(format!(
                    "caller pid {peer_pid} descends from a pane whose agent cannot be named"
                )));
            }
            // The master is a connection-derived agent like any other
            // (CAD-1586): it petitions and runs grant-bound operations
            // under its own subject — never a free pass and never a
            // blanket refusal.
            return Ok(match self.settings_agent_subject(alias, peer_pid)? {
                Some(s) => SettingsCaller::Agent(s),
                None => {
                    SettingsCaller::Unproven(format!("pane agent '{alias}' has no registered row"))
                }
            });
        }
        match self.operator_evidence(peer_pid) {
            Ok(()) => Ok(SettingsCaller::Operator),
            Err(why) => Ok(SettingsCaller::Unproven(why)),
        }
    }

    /// The subject a grant binds for a locally-registered agent: its
    /// alias, its registration generation (reopen/re-register ends it)
    /// and the socket peer's kernel uid. `None` when the alias names no
    /// registered agent.
    fn settings_agent_subject(&self, alias: &str, peer_pid: u32) -> Result<Option<AgentSubject>> {
        let Some(agent) = self.store.agent_opt(alias)? else {
            return Ok(None);
        };
        let generation = agent_registration_generation(&agent);
        let uid = super::serve::frame_peer_uid(self.agent_uid).unwrap_or_else(|_| {
            if cfg!(target_os = "linux") {
                peer_so_uid(peer_pid).unwrap_or_else(|| unsafe { libc::geteuid() })
            } else {
                unsafe { libc::geteuid() }
            }
        });
        let _ = peer_pid;
        Ok(Some(AgentSubject {
            alias: alias.to_string(),
            generation,
            uid,
        }))
    }
}

#[cfg(target_os = "linux")]
fn peer_so_uid(pid: u32) -> Option<u32> {
    // SO_PEERCRED equivalent read for diagnostics-free binding: the
    // process's real uid from /proc status.
    let text = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn peer_so_uid(_pid: u32) -> Option<u32> {
    None
}

// ---------- params hygiene ----------

/// Fields that must never appear on the local command: identity-shaped
/// names (`by`, `as`, `actor`, `caller`, `operator`, `reviewer`, `pane`,
/// `lane`, `pid`, `owner` — refused by `reject_identity_fields`), the
/// `agent` field (requester identity is connection-derived) and the
/// relay's `cli_envelope` (a local caller never presents one — its
/// presence on this socket means a caller is trying to ride the remote
/// authority channel).
fn reject_command_fields(params: &Value) -> Result<()> {
    reject_identity_fields(params, "agent settings command")?;
    for field in ["agent", "cli_envelope"] {
        if params.get(field).is_some() {
            return Err(Error::rejected(format!(
                "agent settings command: request field '{field}' is not accepted"
            )));
        }
    }
    Ok(())
}

fn payload_of(params: &Value) -> Result<&Value> {
    match params.get("payload") {
        None | Some(Value::Null) => Ok(&Value::Null),
        Some(p) => Ok(p),
    }
}

/// Every payload key must be in `allowed`; `grant_id` and
/// `payload_sha256` are always legal on op payloads.
fn check_payload(payload: &Value, allowed: &[&str], op: &str) -> Result<()> {
    let object = payload
        .as_object()
        .ok_or_else(|| Error::rejected(format!("{op} payload must be an object")))?;
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) && key != "grant_id" && key != "payload_sha256" {
            return Err(Error::rejected(format!(
                "{op} payload field '{key}' is not accepted"
            )));
        }
    }
    Ok(())
}

fn need_str<'a>(payload: &'a Value, field: &str, op: &str) -> Result<&'a str> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            Error::invalid(
                "invalid_request",
                format!("{op} requires payload field '{field}'"),
            )
        })
}

fn opt_bool(payload: &Value, field: &str, op: &str) -> Result<Option<bool>> {
    match payload.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        _ => Err(Error::invalid(
            "invalid_request",
            format!("{op} payload field '{field}' must be a boolean or null"),
        )),
    }
}

// ---------- config access ----------

/// One authoritative read: the presented config, the revision token
/// and the live observations — a single `agent_settings_get` snapshot
/// so rows and observations can never mix revisions.
fn current_view(shared: &Arc<Shared>) -> Result<(String, Value, Value)> {
    let snapshot = shared.rpc_agent_settings_get()?;
    let token = snapshot["revision"]
        .as_str()
        .ok_or_else(|| Error::internal("settings snapshot has no revision token"))?
        .to_string();
    let config = snapshot["config"].clone();
    let observations = snapshot["observations"].clone();
    Ok((token, config, observations))
}

/// Mutations need only the token + config from the one snapshot.
fn current_config(shared: &Arc<Shared>) -> Result<(String, Value)> {
    let (token, config, _) = current_view(shared)?;
    Ok((token, config))
}

/// `.put` merge: only the payload's supplied fields land in the row —
/// a mutation grant stays usable without an approved snapshot read,
/// and the digest binds exactly the requested patch. On an existing
/// row absent fields preserve; on a new row `defaults` supplies them
/// or the create refuses.
fn merge_row(
    config: &mut Value,
    table: &str,
    payload: &Value,
    op: &str,
    defaults: impl FnOnce(&Value, Option<&Value>) -> Result<Value>,
) -> Result<()> {
    let id = need_str(payload, "id", op)?;
    let rows = config
        .get_mut(table)
        .and_then(Value::as_array_mut)
        .ok_or_else(|| Error::invalid("invalid_config", format!("config.{table} is missing")))?;
    let position = rows
        .iter()
        .position(|row| row.get("id").and_then(Value::as_str) == Some(id));
    let existing = position.map(|i| rows[i].clone());
    let merged = defaults(payload, existing.as_ref())?;
    match position {
        Some(i) => rows[i] = merged,
        None => rows.push(merged),
    }
    Ok(())
}

/// Required-on-create string: supplied wins, then the stored row, else
/// a refusal that names the field.
fn merge_str(payload: &Value, existing: Option<&Value>, field: &str, op: &str) -> Result<String> {
    if let Some(v) = payload.get(field) {
        return match v.as_str() {
            Some(s) if !s.is_empty() => Ok(s.to_string()),
            _ => Err(Error::invalid(
                "invalid_request",
                format!("{op} payload field '{field}' must be a non-empty string"),
            )),
        };
    }
    existing
        .and_then(|row| row.get(field))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            Error::invalid(
                "invalid_request",
                format!("{op} requires payload field '{field}' (no stored value)"),
            )
        })
}

/// Optional string: supplied wins, then stored, then `default`.
fn merge_str_opt(
    payload: &Value,
    existing: Option<&Value>,
    field: &str,
    default: &str,
) -> Result<String> {
    if let Some(v) = payload.get(field) {
        return match v.as_str() {
            Some(s) => Ok(s.to_string()),
            _ => Err(Error::invalid(
                "invalid_request",
                format!("payload field '{field}' must be a string"),
            )),
        };
    }
    Ok(existing
        .and_then(|row| row.get(field))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| default.to_string()))
}

/// Optional bool: supplied wins, then stored, else `None` for the
/// caller to default.
fn merge_bool(
    payload: &Value,
    existing: Option<&Value>,
    field: &str,
    op: &str,
) -> Result<Option<bool>> {
    if let Some(v) = payload.get(field) {
        return match v.as_bool() {
            Some(b) => Ok(Some(b)),
            _ => Err(Error::invalid(
                "invalid_request",
                format!("{op} payload field '{field}' must be a boolean"),
            )),
        };
    }
    Ok(existing
        .and_then(|row| row.get(field))
        .and_then(Value::as_bool))
}

/// Refuse a stale or absent expected revision. `expected` is what the
/// caller observed; `current` is the live token.
fn expect_revision(expected: Option<&str>, current: &str) -> Result<()> {
    match expected {
        Some(token) if token == current => Ok(()),
        Some(token) => Err(Error::conflict(
            0,
            format!("settings revision is {current} (expected {token})"),
        )),
        None => Err(Error::invalid(
            "invalid_request",
            format!("expected_revision is required (current revision is {current})"),
        )),
    }
}

/// What role `role` actually launches with, resolved the way launch
/// resolution resolves it: a stored claim wins only while the target
/// provider's role selector still records that exact model (a reset
/// or native replacement makes the claim stale, not effective); an
/// empty claim inherits — the provider role selector when one exists,
/// then the provider baseline, then provider_default. `claim` is the
/// raw `roleDefaults` string.
fn role_resolution(
    config: &Value,
    defaults: &crate::model_defaults::ModelDefaults,
    pi: Option<&crate::pi_policy::PiPolicy>,
    role: &str,
    claim: &str,
) -> Value {
    let mut rows = serde_json::Map::new();
    // The launch providers a claim or an inherit can land on: the pi
    // policy when a Pi extension provider exists, plus every native
    // provider id.
    let wants_pi = has_pi_provider(config)
        || !claim.is_empty() && launch_provider_of(config, claim).as_deref() == Some("pi");
    if wants_pi {
        // pi defaults are runtime-role shaped: every team role runs as
        // a worker unless it is the master.
        let fallback = pi.and_then(|p| {
            if role == "master" {
                p.models.default.master.clone()
            } else {
                p.models.default.worker.clone()
            }
        });
        rows.insert(
            "pi".to_string(),
            provider_role_row(defaults, "pi", role, fallback),
        );
    }
    for provider in crate::adapter::registry::model_provider_ids() {
        if provider == "pi" {
            continue;
        }
        rows.insert(
            provider.to_string(),
            provider_role_row(defaults, provider, role, None),
        );
    }
    json!({
        "requested": claim,
        "providers": Value::Object(rows),
    })
}

/// The selector state for one (provider, role): the role's own selector
/// wins, else the provider baseline — the same precedence `inherit`
/// applies at launch. `pi_fallback` is the stored pi policy's
/// runtime-role default (the selector map has no pi row; the saved
/// policy — never pm.yaml on disk — is its authority).
fn provider_role_row(
    defaults: &crate::model_defaults::ModelDefaults,
    provider: &str,
    role: &str,
    pi_fallback: Option<String>,
) -> Value {
    if let Some(pi_model) = pi_fallback {
        return json!({
            "model": pi_model,
            "source": crate::model_defaults::SOURCE_PI_POLICY_DEFAULT,
        });
    }
    let Some(entry) = defaults.providers.get(provider) else {
        return json!({"model": Value::Null, "source": crate::model_defaults::SOURCE_PROVIDER_DEFAULT});
    };
    if let Some(selector) = entry.roles.get(role) {
        return match selector {
            crate::model_defaults::ModelSelector::Model { model } => json!({
                "model": model,
                "source": crate::model_defaults::SOURCE_ROLE_DEFAULT,
            }),
            crate::model_defaults::ModelSelector::ProviderDefault => json!({
                "model": Value::Null,
                "source": crate::model_defaults::SOURCE_PROVIDER_DEFAULT,
            }),
        };
    }
    match &entry.default {
        crate::model_defaults::ModelSelector::Model { model } => json!({
            "model": model,
            "source": crate::model_defaults::SOURCE_PROVIDER_BASELINE,
        }),
        crate::model_defaults::ModelSelector::ProviderDefault => json!({
            "model": Value::Null,
            "source": crate::model_defaults::SOURCE_PROVIDER_DEFAULT,
        }),
    }
}

/// Does any provider row carry the "Pi extension" kind — the
/// launch-provider gate for the pi row in the role report.
fn has_pi_provider(config: &Value) -> bool {
    config
        .get("providers")
        .and_then(Value::as_array)
        .map(|providers| {
            providers
                .iter()
                .any(|row| row.get("kind").and_then(Value::as_str) == Some("Pi extension"))
        })
        .unwrap_or(false)
}
/// One provider row's `kind` from the presented config.
fn provider_kind_of<'a>(config: &'a Value, provider: &str) -> Option<&'a str> {
    config
        .get("providers")
        .and_then(Value::as_array)?
        .iter()
        .find(|row| row.get("id").and_then(Value::as_str) == Some(provider))?
        .get("kind")
        .and_then(Value::as_str)
}

/// The launch provider for model id `model` — the `launch_target`
/// mapping read off the presented config: a Pi extension provider's
/// models launch on "pi"; a registered native provider's on itself.
/// `None` when the model or its provider is unknown or the kind can
/// not launch — the claim then cannot resolve anywhere.
fn launch_provider_of(config: &Value, model: &str) -> Option<String> {
    let provider = config
        .get("models")
        .and_then(Value::as_array)?
        .iter()
        .find(|row| row.get("id").and_then(Value::as_str) == Some(model))?
        .get("provider")
        .and_then(Value::as_str)?
        .to_string();
    let kind = config
        .get("providers")
        .and_then(Value::as_array)?
        .iter()
        .find(|row| row.get("id").and_then(Value::as_str) == Some(provider.as_str()))?
        .get("kind")
        .and_then(Value::as_str)?;
    match kind {
        "Pi extension" => Some("pi".to_string()),
        "native CLI"
            if crate::adapter::registry::model_provider_ids().contains(&provider.as_str()) =>
        {
            Some(provider)
        }
        _ => None,
    }
}

// ---------- operations ----------

impl Shared {
    /// `agent_settings_command` — the one resource-command entry.
    /// Caller authority is derived before any op runs; a refusal leaves
    /// no write.
    pub(super) fn rpc_agent_settings_command(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        reject_command_fields(params)?;
        let op = required_str(params, "operation")?;
        if !known_op(op) {
            return Err(Error::invalid(
                "invalid_request",
                format!("unknown settings operation '{op}'"),
            ));
        }
        let expected = optional_str(params, "expected_revision");
        let payload = payload_of(params)?;
        self.settings_op(op, expected, payload, peer_pid)
    }

    /// Run `op` for a classified caller — shared by the local and the
    /// hosted (`cli_settings_command`) path: the caller differs, the
    /// operation + transaction do not.
    fn settings_op(
        self: &Arc<Self>,
        op: &str,
        expected: Option<&str>,
        payload: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let caller = self.settings_caller(peer_pid)?;
        self.settings_op_caller(op, expected, payload, caller, "operator")
    }

    /// The classified-caller half: `issuer` is the recorded provenance
    /// for grant decisions ("operator" locally, "cli:<org>|<sub>" for a
    /// re-verified hosted owner).
    pub(super) fn settings_op_caller(
        self: &Arc<Self>,
        op: &str,
        expected: Option<&str>,
        payload: &Value,
        caller: SettingsCaller,
        issuer: &str,
    ) -> Result<Value> {
        match &caller {
            SettingsCaller::Operator | SettingsCaller::RemoteOwner => {}
            SettingsCaller::Unproven(why) => {
                return Err(Error::rejected(format!(
                    "settings command '{op}' refused: this connection derives no \
                     agent identity and is not provably the operator: {why}"
                )));
            }
            SettingsCaller::GrantExec => {
                // An approved-argv child keeps read visibility — the
                // path it was approved for — but argv never bound the
                // bytes, so mutations and grant verbs need a real
                // settings grant, which is bound to an agent subject.
                // `master.status` is a settings.read-class op: an
                // approved `cadence master status` child may run it —
                // a smuggled grant_id in argv binds a different subject
                // and fails the grant check anyway.
                if !is_read_op(op) && op != "master.status" {
                    return Err(Error::rejected(format!(
                        "settings command '{op}' refused: an approved-command \
                         connection may read settings but never mutate them or \
                         decide grants — the approval bound argv, not payload \
                         bytes (CAD-1586)"
                    )));
                }
            }
            SettingsCaller::Agent(subject) => {
                if GRANT_ASK_OPS.contains(&op) {
                    // handled below — petition and self-scoped list
                } else if GRANT_DECISION_OPS.contains(&op) {
                    return Err(Error::rejected(format!(
                        "settings command '{op}' refused: grant decisions are \
                         the operator's — agent '{}' may request a grant, never \
                         issue one",
                        subject.alias
                    )));
                } else {
                    // Any other op for an agent connection needs a
                    // grant bound to this exact subject + payload +
                    // revision. `grant_id` travels inside the payload.
                    let bound = GrantSubject::Agent {
                        alias: subject.alias.clone(),
                        generation: subject.generation.clone(),
                        uid: subject.uid,
                    };
                    self.settings_use_grant(
                        &bound,
                        &subject.alias,
                        LOCAL_ORG,
                        op,
                        expected,
                        payload,
                    )?;
                }
            }
            SettingsCaller::RemoteAgent { actor, sub, org } => {
                if GRANT_ASK_OPS.contains(&op) {
                    // petition and self-scoped list — bound cli-agent
                } else if GRANT_DECISION_OPS.contains(&op) {
                    return Err(Error::rejected(format!(
                        "settings command '{op}' refused: grant decisions are \
                         the operator's — a hosted agent principal may request \
                         a grant, never issue one"
                    )));
                } else {
                    let bound = GrantSubject::CliAgent {
                        actor: actor.clone(),
                        sub: sub.clone(),
                    };
                    self.settings_use_grant(&bound, actor, org, op, expected, payload)?;
                }
            }
        }
        match op {
            // ---- petitions and decisions ----
            // Decision ops take the caller's observed revision when one
            // is supplied: the operator approving against a stale view
            // is a conflict, not a silent write (an absent token stays
            // unbound — the CLI sends one, tests send none).
            "grant.request" => self.settings_grant_request(&caller, payload),
            "grant.list" => self.settings_grant_list(&caller),
            "grant.allow" | "grant.reject" | "grant.revoke" => {
                if expected.is_some() {
                    let (token, _) = current_config(self)?;
                    expect_revision(expected, &token)?;
                }
                match op {
                    "grant.allow" => self.settings_grant_allow(payload, issuer),
                    "grant.reject" => self.settings_grant_reject(payload),
                    _ => self.settings_grant_revoke(payload),
                }
            }
            // ---- reads ----
            "snapshot" => self.rpc_agent_settings_get(),
            "history" => {
                let snapshot = self.rpc_agent_settings_get()?;
                Ok(json!({
                    "history": snapshot["history"].clone(),
                    "revision": snapshot["revision"].clone(),
                }))
            }
            "harness.list" => self.read_list("harnesses"),
            "harness.show" => self.read_show("harnesses", "harness", payload),
            "provider.list" => self.read_list("providers"),
            "provider.show" => self.read_show("providers", "provider", payload),
            "model.list" => self.read_models(),
            "model.show" => self.read_model_show(payload),
            "role.list" => self.read_roles(),
            "profile.list" => self.read_profiles(),
            "profile.show" => self.read_profile_show(payload),
            "pin.list" => self.read_list("pins"),
            "profile.preview" => {
                check_payload(payload, &["profile", "harness"], op)?;
                let (token, _) = current_config(self)?;
                expect_revision(expected, &token)?;
                let pm = self.pm()?;
                crate::agent_settings::preview(
                    &pm.dir,
                    &self.store,
                    &token,
                    need_str(payload, "profile", op)?,
                    need_str(payload, "harness", op)?,
                )
            }
            // CAD-1586: master launch diagnosis — a settings-scoped
            // operation (settings.read envelope). It is deliberately
            // NOT in READ_OPS: ordinary agents need a grant to read
            // master internals; the operator reads it free. Not
            // revision-bound and takes no payload fields beyond
            // grant_id/payload_sha256.
            "master.status" => {
                check_payload(payload, &[], op)?;
                self.master_status_document()
            }
            // CAD-1586: registered-but-unlaunchable master recovery —
            // the bounded lifecycle op (try_resume verbatim). A write-class
            // op (settings.write scope for a remote peer) that is not
            // revision-bound; agents reach it only through a bound
            // settings grant; a grant_exec argv can never run it —
            // `master resume` is never-listed AND refused here.
            "master.resume" => {
                check_payload(payload, &[], op)?;
                if expected.is_some() {
                    return Err(Error::invalid(
                        "invalid_request",
                        "master.resume takes no expected_revision",
                    ));
                }
                self.master_resume()
            }
            "catalog.refresh" => {
                check_payload(payload, &["provider"], op)?;
                let pm = self.pm()?;
                crate::agent_settings::catalog(&pm.dir, need_str(payload, "provider", op)?)
            }
            // ---- mutations ----
            "apply" => {
                check_payload(payload, &["config"], op)?;
                let config = payload.get("config").cloned().ok_or_else(|| {
                    Error::invalid("invalid_request", "apply requires payload field 'config'")
                })?;
                self.settings_apply(expected, config, payload)
            }
            "harness.put" => {
                check_payload(payload, &["id", "label", "cli", "note"], op)?;
                self.settings_patch(expected, payload, |config, p| {
                    merge_row(config, "harnesses", p, op, |p, existing| {
                        Ok(json!({
                            "id": need_str(p, "id", op)?,
                            "label": merge_str(p, existing, "label", op)?,
                            "cli": merge_str(p, existing, "cli", op)?,
                            "note": merge_str_opt(p, existing, "note", "")?,
                        }))
                    })
                })
            }
            "harness.remove" => {
                check_payload(payload, &["id"], op)?;
                self.settings_patch(expected, payload, |config, p| {
                    remove_row(config, "harnesses", need_str(p, "id", op)?)
                })
            }
            "provider.put" => {
                check_payload(
                    payload,
                    &["id", "label", "kind", "pin", "gate", "paid", "note"],
                    op,
                )?;
                self.settings_patch(expected, payload, |config, p| {
                    merge_row(config, "providers", p, op, |p, existing| {
                        let kind = merge_str(p, existing, "kind", op)?;
                        // A create defaults gate to the kind's Pi
                        // extension shape and paid to false — an
                        // absent flag preserves the stored value.
                        let gate_default = kind == "Pi extension";
                        Ok(json!({
                            "id": need_str(p, "id", op)?,
                            "label": merge_str(p, existing, "label", op)?,
                            "kind": kind,
                            "pin": merge_str_opt(p, existing, "pin", "")?,
                            "gate": merge_bool(p, existing, "gate", op)?.unwrap_or(gate_default),
                            "paid": merge_bool(p, existing, "paid", op)?.unwrap_or(false),
                            "note": merge_str_opt(p, existing, "note", "")?,
                        }))
                    })
                })
            }
            "provider.remove" => {
                check_payload(payload, &["id"], op)?;
                self.settings_patch(expected, payload, |config, p| {
                    let id = need_str(p, "id", op)?;
                    let referenced = config
                        .get("models")
                        .and_then(Value::as_array)
                        .map(|rows| {
                            rows.iter()
                                .filter(|m| m.get("provider").and_then(Value::as_str) == Some(id))
                                .filter_map(|m| m.get("id").and_then(Value::as_str))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    if !referenced.is_empty() {
                        return Err(Error::rejected(format!(
                            "provider '{id}' still has models {} — remove them first",
                            referenced.join(", ")
                        )));
                    }
                    remove_row(config, "providers", id)
                })
            }
            "model.put" => {
                check_payload(
                    payload,
                    &[
                        "id", "provider", "ctx", "out", "thinking", "images", "scope",
                    ],
                    op,
                )?;
                self.settings_patch(expected, payload, |config, p| {
                    let id = need_str(p, "id", op)?.to_string();
                    merge_row(config, "models", p, op, |p, existing| {
                        // Catalog facts merge as tri-state: an absent
                        // field preserves the stored value (unknown
                        // stays unknown, never coerced false); a new
                        // row defaults to null.
                        let thinking = match opt_bool(p, "thinking", op)? {
                            Some(v) => json!(v),
                            None => existing
                                .and_then(|row| row.get("thinking").cloned())
                                .unwrap_or(Value::Null),
                        };
                        let images = match opt_bool(p, "images", op)? {
                            Some(v) => json!(v),
                            None => existing
                                .and_then(|row| row.get("images").cloned())
                                .unwrap_or(Value::Null),
                        };
                        Ok(json!({
                            "id": id.clone(),
                            "provider": merge_str(p, existing, "provider", op)?,
                            "ctx": merge_str_opt(p, existing, "ctx", "")?,
                            "out": merge_str_opt(p, existing, "out", "")?,
                            "thinking": thinking,
                            "images": images,
                        }))
                    })?;
                    let gated = config["models"]
                        .as_array()
                        .and_then(|rows| rows.iter().find(|row| row["id"].as_str() == Some(&id)))
                        .and_then(|row| row["provider"].as_str())
                        .and_then(|provider| provider_kind_of(config, provider))
                        == Some("Pi extension");
                    let explicit_scope = if p.get("scope").is_some() {
                        if !gated {
                            return Err(Error::rejected(
                                "Pi launch scope does not apply to native-CLI models",
                            ));
                        }
                        Some(need_str(p, "scope", op)?)
                    } else {
                        None
                    };
                    if gated {
                        let scopes = config
                            .get_mut("scope")
                            .and_then(Value::as_object_mut)
                            .ok_or_else(|| {
                                Error::invalid("invalid_config", "config.scope is missing")
                            })?;
                        if let Some(scope) = explicit_scope {
                            scopes.insert(id, json!(scope));
                        } else {
                            scopes.entry(id).or_insert_with(|| json!("off"));
                        }
                    }
                    Ok(())
                })
            }
            "model.rename" => {
                check_payload(payload, &["id", "to"], op)?;
                self.settings_patch(expected, payload, |config, p| {
                    let id = need_str(p, "id", op)?;
                    let to = need_str(p, "to", op)?;
                    let models = config
                        .get_mut("models")
                        .and_then(Value::as_array_mut)
                        .ok_or_else(|| {
                            Error::invalid("invalid_config", "config.models is missing")
                        })?;
                    if models
                        .iter()
                        .any(|row| row.get("id").and_then(Value::as_str) == Some(to))
                    {
                        return Err(Error::rejected(format!(
                            "model rename refused: '{to}' already exists"
                        )));
                    }
                    let row = models
                        .iter_mut()
                        .find(|row| row.get("id").and_then(Value::as_str) == Some(id))
                        .ok_or_else(|| {
                            Error::invalid("invalid_request", format!("unknown model '{id}'"))
                        })?;
                    row["id"] = json!(to);
                    // Every reference moves with the id in the same
                    // transaction: launch scope, role defaults, and
                    // explicit profile models.
                    if let Some(scope) = config.get_mut("scope").and_then(Value::as_object_mut) {
                        if let Some(mode) = scope.remove(id) {
                            scope.insert(to.to_string(), mode);
                        }
                    }
                    if let Some(roles) = config
                        .get_mut("roleDefaults")
                        .and_then(Value::as_object_mut)
                    {
                        for (_, value) in roles.iter_mut() {
                            if value.as_str() == Some(id) {
                                *value = json!(to);
                            }
                        }
                    }
                    if let Some(profiles) = config.get_mut("profiles").and_then(Value::as_array_mut)
                    {
                        for profile in profiles.iter_mut() {
                            if profile.get("model").and_then(Value::as_str) == Some(id) {
                                profile["model"] = json!(to);
                            }
                        }
                    }
                    Ok(())
                })
            }
            "model.remove" => {
                check_payload(payload, &["id"], op)?;
                self.settings_patch(expected, payload, |config, p| {
                    let id = need_str(p, "id", op)?;
                    if let Some(scope) = config.get_mut("scope").and_then(Value::as_object_mut) {
                        scope.remove(id);
                    }
                    remove_row(config, "models", id)
                })
            }
            "model.scope" => {
                check_payload(payload, &["id", "scope"], op)?;
                self.settings_patch(expected, payload, |config, p| {
                    let id = need_str(p, "id", op)?;
                    let mode = need_str(p, "scope", op)?;
                    if !matches!(mode, "off" | "allow" | "master" | "worker") {
                        return Err(Error::invalid(
                            "invalid_request",
                            "scope must be off, allow, master, or worker",
                        ));
                    }
                    let provider = config
                        .get("models")
                        .and_then(Value::as_array)
                        .and_then(|rows| {
                            rows.iter()
                                .find(|row| row.get("id").and_then(Value::as_str) == Some(id))
                                .and_then(|row| row.get("provider").and_then(Value::as_str))
                                .map(str::to_string)
                        })
                        .ok_or_else(|| {
                            Error::invalid("invalid_request", format!("unknown model '{id}'"))
                        })?;
                    let kind = config
                        .get("providers")
                        .and_then(Value::as_array)
                        .and_then(|rows| {
                            rows.iter()
                                .find(|row| {
                                    row.get("id").and_then(Value::as_str) == Some(provider.as_str())
                                })
                                .and_then(|row| row.get("kind").and_then(Value::as_str))
                                .map(str::to_string)
                        })
                        .unwrap_or_default();
                    if kind == "native CLI" {
                        return Err(Error::rejected(format!(
                            "model '{id}' is a native-CLI provider model — the Pi \
                             launch scope does not apply to it"
                        )));
                    }
                    config
                        .get_mut("scope")
                        .and_then(Value::as_object_mut)
                        .ok_or_else(|| Error::invalid("invalid_config", "config.scope is missing"))?
                        .insert(id.to_string(), json!(mode));
                    Ok(())
                })
            }
            "catalog.import" => {
                check_payload(payload, &["provider", "models"], op)?;
                let provider = need_str(payload, "provider", op)?.to_string();
                let selection = match payload.get("models") {
                    Some(Value::String(s)) if s == "all-new" => CatalogSelection::AllNew,
                    Some(Value::Array(items)) => CatalogSelection::Ids(
                        items
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect(),
                    ),
                    _ => {
                        return Err(Error::invalid(
                            "invalid_request",
                            "catalog.import requires payload 'models': an array of \
                             ids or \"all-new\"",
                        ))
                    }
                };
                self.settings_catalog_import(expected, payload, &provider, selection)
            }
            "role.set" => {
                check_payload(payload, &["role", "model"], op)?;
                self.settings_patch(expected, payload, |config, p| {
                    let role = need_str(p, "role", op)?;
                    let roles = config
                        .get_mut("roleDefaults")
                        .and_then(Value::as_object_mut)
                        .ok_or_else(|| {
                            Error::invalid("invalid_config", "config.roleDefaults is missing")
                        })?;
                    if !roles.contains_key(role) {
                        return Err(Error::invalid(
                            "invalid_request",
                            format!("unknown team role '{role}'"),
                        ));
                    }
                    roles.insert(role.to_string(), json!(need_str(p, "model", op)?));
                    Ok(())
                })
            }
            "role.clear" => {
                check_payload(payload, &["role"], op)?;
                self.settings_patch(expected, payload, |config, p| {
                    let role = need_str(p, "role", op)?;
                    let roles = config
                        .get_mut("roleDefaults")
                        .and_then(Value::as_object_mut)
                        .ok_or_else(|| {
                            Error::invalid("invalid_config", "config.roleDefaults is missing")
                        })?;
                    if !roles.contains_key(role) {
                        return Err(Error::invalid(
                            "invalid_request",
                            format!("unknown team role '{role}'"),
                        ));
                    }
                    roles.insert(role.to_string(), json!(""));
                    Ok(())
                })
            }
            "profile.put" => {
                check_payload(payload, &["id", "role", "model", "job"], op)?;
                self.settings_patch(expected, payload, |config, p| {
                    merge_row(config, "profiles", p, op, |p, existing| {
                        Ok(json!({
                            "id": need_str(p, "id", op)?,
                            "role": merge_str(p, existing, "role", op)?,
                            "model": merge_str_opt(p, existing, "model", "inherit")?,
                            "job": merge_str(p, existing, "job", op)?,
                        }))
                    })
                })
            }
            "profile.rename" => {
                check_payload(payload, &["id", "to"], op)?;
                self.settings_patch(expected, payload, |config, p| {
                    let id = need_str(p, "id", op)?;
                    let to = need_str(p, "to", op)?;
                    let profiles = config
                        .get_mut("profiles")
                        .and_then(Value::as_array_mut)
                        .ok_or_else(|| {
                            Error::invalid("invalid_config", "config.profiles is missing")
                        })?;
                    if profiles
                        .iter()
                        .any(|row| row.get("id").and_then(Value::as_str) == Some(to))
                    {
                        return Err(Error::rejected(format!(
                            "profile rename refused: '{to}' already exists"
                        )));
                    }
                    let row = profiles
                        .iter_mut()
                        .find(|row| row.get("id").and_then(Value::as_str) == Some(id))
                        .ok_or_else(|| {
                            Error::invalid("invalid_request", format!("unknown profile '{id}'"))
                        })?;
                    row["id"] = json!(to);
                    Ok(())
                })
            }
            "profile.remove" => {
                check_payload(payload, &["id"], op)?;
                self.settings_patch(expected, payload, |config, p| {
                    remove_row(config, "profiles", need_str(p, "id", op)?)
                })
            }
            "profile.sync" => {
                check_payload(payload, &["profile"], op)?;
                let (token, _) = current_config(self)?;
                expect_revision(expected, &token)?;
                let profile = match payload.get("profile") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(id)) => Some(id.as_str()),
                    Some(_) => {
                        return Err(Error::invalid(
                            "invalid_request",
                            "'profile' must be a string or null",
                        ))
                    }
                };
                let agents = self.store.agents()?;
                let pm = self.pm()?;
                crate::agent_settings::sync(&pm, &self.store, &agents, &token, profile)
            }
            "pin.put" => {
                check_payload(payload, &["pkg", "pinned"], op)?;
                self.settings_patch(expected, payload, |config, p| {
                    let pkg = need_str(p, "pkg", op)?;
                    let pins = config
                        .get_mut("pins")
                        .and_then(Value::as_array_mut)
                        .ok_or_else(|| {
                            Error::invalid("invalid_config", "config.pins is missing")
                        })?;
                    let row = json!({"pkg": pkg, "pinned": need_str(p, "pinned", op)?});
                    match pins
                        .iter()
                        .position(|pin| pin.get("pkg").and_then(Value::as_str) == Some(pkg))
                    {
                        Some(i) => pins[i] = row,
                        None => pins.push(row),
                    }
                    Ok(())
                })
            }
            "pin.remove" => {
                check_payload(payload, &["pkg"], op)?;
                self.settings_patch(expected, payload, |config, p| {
                    let pkg = need_str(p, "pkg", op)?;
                    let pins = config
                        .get_mut("pins")
                        .and_then(Value::as_array_mut)
                        .ok_or_else(|| {
                            Error::invalid("invalid_config", "config.pins is missing")
                        })?;
                    let before = pins.len();
                    pins.retain(|pin| pin.get("pkg").and_then(Value::as_str) != Some(pkg));
                    if pins.len() == before {
                        return Err(Error::invalid(
                            "invalid_request",
                            format!("unknown pin '{pkg}'"),
                        ));
                    }
                    Ok(())
                })
            }
            _ => Err(Error::internal(format!(
                "settings operation '{op}' is not implemented"
            ))),
        }
    }

    /// Patch + save: load the live config and token, require the
    /// caller's observed revision, run `patch` on the config object,
    /// verify a declared `payload_sha256` when the caller pinned one,
    /// then commit through the UI's own transaction.
    fn settings_patch(
        self: &Arc<Self>,
        expected: Option<&str>,
        payload: &Value,
        patch: impl FnOnce(&mut Value, &Value) -> Result<()>,
    ) -> Result<Value> {
        let (token, mut config) = current_config(self)?;
        expect_revision(expected, &token)?;
        patch(&mut config, payload)?;
        self.verify_declared_digest(payload)?;
        self.settings_apply_unchecked(&token, config)
    }

    /// `apply` — the full document, byte-bound when the caller declared
    /// a digest.
    fn settings_apply(
        self: &Arc<Self>,
        expected: Option<&str>,
        config: Value,
        payload: &Value,
    ) -> Result<Value> {
        let (token, _) = current_config(self)?;
        expect_revision(expected, &token)?;
        self.verify_declared_digest(payload)?;
        self.settings_apply_unchecked(&token, config)
    }

    /// A `payload_sha256` the caller pinned must match the digest the
    /// daemon computes over the payload minus its envelope fields —
    /// the flag in approved argv binds the same bytes the grant binds.
    fn verify_declared_digest(&self, payload: &Value) -> Result<()> {
        let Some(declared) = payload.get("payload_sha256").and_then(Value::as_str) else {
            return Ok(());
        };
        let mut body = payload.clone();
        if let Some(object) = body.as_object_mut() {
            object.remove("payload_sha256");
            object.remove("grant_id");
        }
        let actual = payload_digest(&body);
        if declared != actual {
            return Err(Error::rejected(format!(
                "payload digest mismatch: the sent payload does not match \
                 payload_sha256 {declared} — a replaced file or edited field is \
                 refused before any mutation"
            )));
        }
        Ok(())
    }

    /// Commit a patched config through the exact UI save path.
    fn settings_apply_unchecked(self: &Arc<Self>, token: &str, config: Value) -> Result<Value> {
        let agents = self.store.agents()?;
        let pm = self.pm()?;
        crate::agent_settings::save(&pm, &self.state_dir, &self.store, &agents, token, &config)
    }

    /// `catalog.import` — the daemon recomputes the provider catalog in
    /// this call (a stale client copy can never import), then adds the
    /// selected/new rows in the same settings transaction. Gated
    /// provider models land scope `off`; discovery never authorizes.
    fn settings_catalog_import(
        self: &Arc<Self>,
        expected: Option<&str>,
        payload: &Value,
        provider: &str,
        selection: CatalogSelection,
    ) -> Result<Value> {
        let (token, mut config) = current_config(self)?;
        expect_revision(expected, &token)?;
        let pm = self.pm()?;
        let catalog = crate::agent_settings::catalog(&pm.dir, provider)?;
        if let Some(error) = catalog.get("error").and_then(Value::as_str) {
            return Err(Error::rejected(format!(
                "catalog for provider '{provider}' is unavailable: {error}"
            )));
        }
        let rows = catalog
            .get("models")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let kind = config
            .get("providers")
            .and_then(Value::as_array)
            .and_then(|providers| {
                providers
                    .iter()
                    .find(|row| row.get("id").and_then(Value::as_str) == Some(provider))
                    .and_then(|row| row.get("kind").and_then(Value::as_str))
                    .map(str::to_string)
            })
            .ok_or_else(|| {
                Error::invalid("invalid_request", format!("unknown provider '{provider}'"))
            })?;
        let models = config
            .get_mut("models")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| Error::invalid("invalid_config", "config.models is missing"))?;
        let mut imported = Vec::new();
        let mut gated = Vec::new();
        for row in rows {
            let Some(id) = row.get("id").and_then(Value::as_str) else {
                continue;
            };
            let already = models
                .iter()
                .any(|m| m.get("id").and_then(Value::as_str) == Some(id));
            let wanted = match &selection {
                CatalogSelection::AllNew => !already,
                CatalogSelection::Ids(ids) => ids.iter().any(|want| want == id),
            };
            if !wanted || already {
                continue;
            }
            models.push(json!({
                "id": id,
                "provider": provider,
                "ctx": row.get("ctx").and_then(Value::as_str).unwrap_or(""),
                "out": row.get("out").and_then(Value::as_str).unwrap_or(""),
                "thinking": row.get("thinking").cloned().unwrap_or(Value::Null),
                "images": row.get("images").cloned().unwrap_or(Value::Null),
            }));
            imported.push(id.to_string());
            // Discovery never authorizes: gated models import off.
            if kind == "Pi extension" {
                gated.push(id.to_string());
            }
        }
        if !gated.is_empty() {
            let scope = config
                .get_mut("scope")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| Error::invalid("invalid_config", "config.scope is missing"))?;
            for id in gated {
                scope.insert(id, json!("off"));
            }
        }
        self.verify_declared_digest(payload)?;
        let snapshot = self.settings_apply_unchecked(&token, config)?;
        Ok(json!({"snapshot": snapshot, "imported": imported}))
    }

    // ---------- reads ----------

    /// List `config[key]` rows with that table's observations — one
    /// snapshot, so the rows and the observation map share a revision.
    /// Tables with no observation map (providers, pins) answer `{}`.
    fn read_list(self: &Arc<Self>, key: &str) -> Result<Value> {
        let (token, config, observations) = current_view(self)?;
        let rows = config.get(key).cloned().unwrap_or(json!([]));
        Ok(json!({
            key: rows,
            "observations": observations.get(key).cloned().unwrap_or(json!({})),
            "revision": token,
        }))
    }

    fn read_show(self: &Arc<Self>, key: &str, name: &str, payload: &Value) -> Result<Value> {
        check_payload(payload, &["id"], "show")?;
        let id = need_str(payload, "id", "show")?;
        let (token, config, observations) = current_view(self)?;
        let rows = config
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let row = rows
            .iter()
            .find(|row| row.get("id").and_then(Value::as_str) == Some(id))
            .cloned()
            .ok_or_else(|| Error::invalid("invalid_request", format!("unknown {name} '{id}'")))?;
        let obs = observations.get(key).cloned().unwrap_or(json!({}));
        Ok(json!({
            name: row,
            "observation": obs[id].clone(),
            "revision": token,
        }))
    }

    /// `model.list` — the config rows plus scope, live observations and
    /// the effective runtime-role gate: `pi_gated` marks a Pi
    /// extension provider's models; `allowed_roles` names the
    /// `master`/`worker` runtime roles the stored pi policy allows the
    /// model to — native CLI providers are never Pi-gated. Rows and
    /// observations come from one snapshot; the allowlists are the same
    /// saved policy a launch checks.
    fn read_models(self: &Arc<Self>) -> Result<Value> {
        let (token, config, observations) = current_view(self)?;
        let scope = config
            .get("scope")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let roles = config
            .get("roleDefaults")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let obs = observations.get("models").cloned().unwrap_or(json!({}));
        let pi = self.store.settings()?.doc.pi;
        let master_allow: Vec<String> = pi
            .as_ref()
            .map(|p| p.models.allow_for("master").to_vec())
            .unwrap_or_default();
        let worker_allow: Vec<String> = pi
            .as_ref()
            .map(|p| p.models.allow_for("worker").to_vec())
            .unwrap_or_default();
        let models = config
            .get("models")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|row| {
                let id = row.get("id").and_then(Value::as_str).unwrap_or("");
                let used: Vec<&String> = roles
                    .iter()
                    .filter(|(_, model)| model.as_str() == Some(id))
                    .map(|(role, _)| role)
                    .collect();
                let launch_scope = scope.get(id).cloned().unwrap_or(Value::Null);
                let live = obs[id]["live"].clone();
                // A Pi extension provider's models are gated by the
                // stored policy's runtime-role allowlists; a native
                // provider's never are (no list applies).
                let gated = provider_kind_of(
                    &config,
                    row.get("provider").and_then(Value::as_str).unwrap_or(""),
                ) == Some("Pi extension");
                let allowed_roles: Vec<&str> = if gated {
                    ["master", "worker"]
                        .into_iter()
                        .filter(|role| {
                            let list = if *role == "master" {
                                &master_allow
                            } else {
                                &worker_allow
                            };
                            list.iter().any(|m| m == id)
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                let mut row = row;
                row["scope"] = launch_scope;
                row["live"] = live;
                row["roleDefaults"] = json!(used);
                row["pi_gated"] = json!(gated);
                row["allowed_roles"] = json!(allowed_roles);
                row
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "models": models,
            "pi_allowlists": {
                "master": master_allow,
                "worker": worker_allow,
            },
            "revision": token,
        }))
    }

    fn read_model_show(self: &Arc<Self>, payload: &Value) -> Result<Value> {
        check_payload(payload, &["id"], "model.show")?;
        let id = need_str(payload, "id", "model.show")?;
        let listed = self.read_models()?;
        let row = listed["models"]
            .as_array()
            .and_then(|rows| {
                rows.iter()
                    .find(|row| row.get("id").and_then(Value::as_str) == Some(id))
                    .cloned()
            })
            .ok_or_else(|| Error::invalid("invalid_request", format!("unknown model '{id}'")))?;
        Ok(json!({"model": row, "revision": listed["revision"].clone()}))
    }

    /// `role.list` — the stored claims plus what each role actually
    /// resolves to. A `roleDefaults` value is a claim, not the answer:
    /// an empty one inherits and a stale one survives `save`'s keep
    /// only while the provider role still records it. The effective
    /// rows are resolved against the saved model-defaults document
    /// (the selector map `resolve` consults at launch) and the pi
    /// policy's worker default — never the raw string echoed back.
    fn read_roles(self: &Arc<Self>) -> Result<Value> {
        let (token, config) = current_config(self)?;
        // One stored-settings read supplies the selector map and the pi
        // policy — the same saved document a launch resolves against;
        // pm.yaml on disk can drift and a read error surfaces, never
        // falls back.
        let saved = self.store.settings()?;
        let defaults = &saved.doc.model_defaults;
        let pi = &saved.doc.pi;
        let requested = config
            .get("roleDefaults")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut effective = serde_json::Map::new();
        for (role, _) in crate::model_defaults::TEAM_ROLES {
            let claim = requested.get(*role).and_then(Value::as_str).unwrap_or("");
            effective.insert(
                (*role).to_string(),
                role_resolution(&config, defaults, pi.as_ref(), role, claim),
            );
        }
        Ok(json!({
            "roleDefaults": Value::Object(requested),
            "effective": Value::Object(effective),
            "revision": token,
        }))
    }

    fn read_profiles(self: &Arc<Self>) -> Result<Value> {
        let (token, config, observations) = current_view(self)?;
        let obs = observations.get("profiles").cloned().unwrap_or(json!({}));
        let profiles = config
            .get("profiles")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|row| {
                let id = row.get("id").and_then(Value::as_str).unwrap_or("");
                let observation = obs[id].clone();
                let mut row = row;
                row["observation"] = observation;
                row
            })
            .collect::<Vec<_>>();
        Ok(json!({"profiles": profiles, "revision": token}))
    }

    fn read_profile_show(self: &Arc<Self>, payload: &Value) -> Result<Value> {
        check_payload(payload, &["id"], "profile.show")?;
        let id = need_str(payload, "id", "profile.show")?;
        let listed = self.read_profiles()?;
        let row = listed["profiles"]
            .as_array()
            .and_then(|rows| {
                rows.iter()
                    .find(|row| row.get("id").and_then(Value::as_str) == Some(id))
                    .cloned()
            })
            .ok_or_else(|| Error::invalid("invalid_request", format!("unknown profile '{id}'")))?;
        Ok(json!({"profile": row, "revision": listed["revision"].clone()}))
    }

    // ---------- grants ----------

    /// `grant.request` — an agent records the exact operation + payload
    /// it wants to run. Identity is bound from the connection; a
    /// request is never authority. Operators have no reason to file
    /// one, and `grant_exec`/unproven callers were refused
    /// before dispatch.
    fn settings_grant_request(&self, caller: &SettingsCaller, payload: &Value) -> Result<Value> {
        check_payload(
            payload,
            &["operation", "expected_revision", "payload", "reason"],
            "grant.request",
        )?;
        let (subject, org) = match caller {
            SettingsCaller::Agent(subject) => (
                GrantSubject::Agent {
                    alias: subject.alias.clone(),
                    generation: subject.generation.clone(),
                    uid: subject.uid,
                },
                LOCAL_ORG.to_string(),
            ),
            SettingsCaller::RemoteAgent { actor, sub, org } => (
                GrantSubject::CliAgent {
                    actor: actor.clone(),
                    sub: sub.clone(),
                },
                org.clone(),
            ),
            _ => {
                return Err(Error::rejected(
                    "grant request refused: only an agent connection or a hosted \
                     agent principal files a settings request — the operator \
                     already holds the authority",
                ))
            }
        };
        let operation = need_str(payload, "operation", "grant.request")?.to_string();
        // Requestable ops are every resource op — grant.* are the only
        // non-requestable names (a settings grant can never confer grant
        // administration; designation stays operator-owned).
        if !known_op(&operation) || operation.starts_with("grant.") {
            return Err(Error::invalid(
                "invalid_request",
                format!("operation '{operation}' cannot be requested"),
            ));
        }
        // Identity/digest fields inside the intended payload are
        // refused the same as on the real command — a grant minted for
        // a forged body would carry the forgery into authority.
        let intended = payload.get("payload").cloned().unwrap_or(Value::Null);
        if let Some(object) = intended.as_object() {
            for field in ["agent", "grant_id", "payload_sha256", "cli_envelope"] {
                if object.contains_key(field) {
                    return Err(Error::rejected(format!(
                        "grant request refused: the intended payload may not \
                         carry '{field}'"
                    )));
                }
            }
        }
        for field in IDENTITY_FIELDS {
            if intended.get(*field).is_some() {
                return Err(Error::rejected(format!(
                    "grant request refused: the intended payload may not carry \
                     identity field '{field}'"
                )));
            }
        }
        let reason = need_str(payload, "reason", "grant.request")?.to_string();
        if reason.len() > 500 {
            return Err(Error::rejected("grant request reason is too long"));
        }
        let expected_revision = payload
            .get("expected_revision")
            .and_then(Value::as_str)
            .map(str::to_string);
        if revision_bound(&operation) && expected_revision.is_none() {
            return Err(Error::invalid(
                "invalid_request",
                format!("operation '{operation}' needs expected_revision in the request"),
            ));
        }
        let now = now_epoch();
        let lock = grants_lock(&self.state_dir);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut doc = grants_load(&self.state_dir)?;
        expire_requests(&mut doc, now);
        let request = SettingsRequest {
            id: format!("sr-{}", uuid::Uuid::new_v4().simple()),
            subject: subject.clone(),
            org,
            operation,
            expected_revision,
            payload: intended.clone(),
            payload_sha256: payload_digest(&intended),
            reason,
            created: now,
            expires_at: now + GRANT_TTL_SECS,
            status: "pending".to_string(),
            decision_at: None,
            grant_id: None,
        };
        let id = request.id.clone();
        doc.requests.push(request);
        grants_save(&self.state_dir, &doc)?;
        Ok(json!({"request_id": id}))
    }

    /// `grant.list` — the operator reads all; an agent reads only its
    /// own petitions and grants.
    fn settings_grant_list(&self, caller: &SettingsCaller) -> Result<Value> {
        let now = now_epoch();
        let lock = grants_lock(&self.state_dir);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut doc = grants_load(&self.state_dir)?;
        expire_requests(&mut doc, now);
        let own_request = |r: &&SettingsRequest| match caller {
            SettingsCaller::Agent(subject) => matches!(
                &r.subject,
                GrantSubject::Agent { alias, .. } if alias == &subject.alias
            ),
            SettingsCaller::RemoteAgent { actor, sub, .. } => matches!(
                &r.subject,
                GrantSubject::CliAgent { actor: a, sub: s } if a == actor && s == sub
            ),
            _ => false,
        };
        let own_grant = |g: &&SettingsGrant| match caller {
            SettingsCaller::Agent(subject) => matches!(
                &g.subject,
                GrantSubject::Agent { alias, .. } if alias == &subject.alias
            ),
            SettingsCaller::RemoteAgent { actor, sub, .. } => matches!(
                &g.subject,
                GrantSubject::CliAgent { actor: a, sub: s } if a == actor && s == sub
            ),
            _ => false,
        };
        match caller {
            SettingsCaller::Operator | SettingsCaller::RemoteOwner => Ok(json!({
                "requests": doc.requests.iter().map(request_json).collect::<Vec<_>>(),
                "grants": doc.grants.iter().map(grant_json).collect::<Vec<_>>(),
            })),
            SettingsCaller::Agent(_) | SettingsCaller::RemoteAgent { .. } => Ok(json!({
                "requests": doc.requests.iter().filter(own_request).map(request_json).collect::<Vec<_>>(),
                "grants": doc.grants.iter().filter(own_grant).map(grant_json).collect::<Vec<_>>(),
            })),
            _ => Err(Error::rejected("grant list refused for this caller")),
        }
    }

    /// `grant.allow` — proven operator only (grant_exec was refused
    /// before dispatch). Approves the request's stored payload as-is:
    /// the grant binds what the agent asked, the revision it named and
    /// the connection identity that asked.
    fn settings_grant_allow(&self, payload: &Value, issuer: &str) -> Result<Value> {
        check_payload(payload, &["request_id"], "grant.allow")?;
        let request_id = need_str(payload, "request_id", "grant.allow")?;
        let now = now_epoch();
        let lock = grants_lock(&self.state_dir);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut doc = grants_load(&self.state_dir)?;
        expire_requests(&mut doc, now);
        let request = doc
            .requests
            .iter_mut()
            .find(|r| r.id == request_id)
            .ok_or_else(|| {
                Error::invalid("invalid_request", format!("unknown request '{request_id}'"))
            })?;
        if request.status != "pending" {
            return Err(Error::rejected(format!(
                "request '{request_id}' is {} — only a pending request can be allowed",
                request.status
            )));
        }
        // A locally-bound requester must still hold the registration it
        // asked under — a re-registered alias ends the request it
        // filed. A hosted subject's liveness is the envelope's own
        // per-call live re-check, not a local row.
        if let GrantSubject::Agent {
            alias, generation, ..
        } = &request.subject
        {
            let still = self
                .store
                .agent_opt(alias)?
                .map(|agent| agent_registration_generation(&agent) == *generation)
                .unwrap_or(false);
            if !still {
                return Err(Error::rejected(format!(
                    "request '{request_id}' names an agent registration that no \
                    longer holds — the agent must request again under its \
                    current identity"
                )));
            }
        }
        let grant = SettingsGrant {
            id: format!("sg-{}", uuid::Uuid::new_v4().simple()),
            request_id: request.id.clone(),
            subject: request.subject.clone(),
            operation: request.operation.clone(),
            expected_revision: request.expected_revision.clone(),
            payload: request.payload.clone(),
            payload_sha256: request.payload_sha256.clone(),
            org: request.org.clone(),
            issued_at: now,
            expires_at: now + GRANT_TTL_SECS,
            revoked: false,
            used_at: None,
            issuer: issuer.to_string(),
        };
        request.status = "allowed".to_string();
        request.decision_at = Some(now);
        request.grant_id = Some(grant.id.clone());
        let bound = json!({
            "operation": grant.operation,
            "payload_sha256": grant.payload_sha256,
            "expected_revision": grant.expected_revision,
            "subject": grant.subject,
            "org": grant.org,
        });
        let answer = json!({
            "grant_id": grant.id,
            "expires_at": grant.expires_at,
            "bound": bound,
        });
        doc.grants.push(grant);
        grants_save(&self.state_dir, &doc)?;
        Ok(answer)
    }

    fn settings_grant_reject(&self, payload: &Value) -> Result<Value> {
        check_payload(payload, &["request_id"], "grant.reject")?;
        let request_id = need_str(payload, "request_id", "grant.reject")?;
        let now = now_epoch();
        let lock = grants_lock(&self.state_dir);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut doc = grants_load(&self.state_dir)?;
        expire_requests(&mut doc, now);
        let request = doc
            .requests
            .iter_mut()
            .find(|r| r.id == request_id)
            .ok_or_else(|| {
                Error::invalid("invalid_request", format!("unknown request '{request_id}'"))
            })?;
        if request.status != "pending" {
            return Err(Error::rejected(format!(
                "request '{request_id}' is {} — only a pending request can be rejected",
                request.status
            )));
        }
        request.status = "rejected".to_string();
        request.decision_at = Some(now);
        grants_save(&self.state_dir, &doc)?;
        Ok(json!({"request_id": request_id, "status": "rejected"}))
    }

    fn settings_grant_revoke(&self, payload: &Value) -> Result<Value> {
        check_payload(payload, &["grant_id"], "grant.revoke")?;
        let grant_id = need_str(payload, "grant_id", "grant.revoke")?;
        let lock = grants_lock(&self.state_dir);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut doc = grants_load(&self.state_dir)?;
        let grant = doc
            .grants
            .iter_mut()
            .find(|g| g.id == grant_id)
            .ok_or_else(|| {
                Error::invalid("invalid_request", format!("unknown grant '{grant_id}'"))
            })?;
        if grant.used_at.is_some() {
            return Err(Error::rejected(format!(
                "grant '{grant_id}' is already consumed — revocation is moot"
            )));
        }
        grant.revoked = true;
        grants_save(&self.state_dir, &doc)?;
        Ok(json!({"grant_id": grant_id, "status": "revoked"}))
    }

    /// The agent-side grant check for a non-ask op: the payload must
    /// carry `grant_id` naming a live grant bound to this exact
    /// subject, operation, canonical payload bytes and expected
    /// revision. The grant is consumed at authorize time — before the
    /// op runs — so a replay can never reach the mutation.
    fn settings_use_grant(
        &self,
        bound: &GrantSubject,
        who: &str,
        org: &str,
        op: &str,
        expected: Option<&str>,
        payload: &Value,
    ) -> Result<()> {
        let grant_id = payload
            .get("grant_id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                Error::rejected(format!(
                    "settings command '{op}' refused for '{who}': no settings \
                 grant — file `grant request` for the exact change and have the \
                 operator approve it (`cadence settings agents grant`)"
                ))
            })?;
        let mut bound_body = payload.clone();
        if let Some(object) = bound_body.as_object_mut() {
            object.remove("grant_id");
        }
        let now = now_epoch();
        let lock = grants_lock(&self.state_dir);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut doc = grants_load(&self.state_dir)?;
        let index = doc
            .grants
            .iter()
            .position(|g| g.id == grant_id)
            .ok_or_else(|| Error::rejected(format!("settings grant '{grant_id}' is unknown")))?;
        let grant = &doc.grants[index];
        let fail = |why: &str| -> Error {
            Error::rejected(format!("settings grant '{grant_id}' refused: {why}"))
        };
        if grant.revoked {
            return Err(fail("it was revoked"));
        }
        if grant.used_at.is_some() {
            return Err(fail("it was already consumed — a grant is single-use"));
        }
        if now >= grant.expires_at {
            return Err(fail("it expired"));
        }
        if grant.org != org {
            return Err(fail("it was minted for a different org"));
        }
        if grant.subject != *bound {
            return Err(fail(
                "this connection is not the grant's bound agent identity",
            ));
        }
        if grant.operation != op {
            return Err(fail(&format!(
                "it authorizes '{}', not '{op}'",
                grant.operation
            )));
        }
        // The received payload must equal the approved one byte for
        // byte (canonical) — a replaced --file or any edited field is
        // a different command.
        if payload_digest(&bound_body) != grant.payload_sha256 {
            return Err(fail(
                "the payload does not match the approved command — the grant \
                 binds the exact change, not just the operation",
            ));
        }
        if grant.expected_revision.as_deref() != expected {
            return Err(fail("the observed revision differs from the approved one"));
        }
        doc.grants[index].used_at = Some(now);
        grants_save(&self.state_dir, &doc)?;
        Ok(())
    }
}

/// A pending request past its decidable window flips to `expired` in
/// place; allowed/rejected rows keep their verdict.
fn expire_requests(doc: &mut GrantDoc, now: i64) {
    for request in &mut doc.requests {
        if request.status == "pending" && now >= request.expires_at {
            request.status = "expired".to_string();
            request.decision_at = Some(now);
        }
    }
}

/// How `catalog.import` picks rows from the recomputed catalog.
enum CatalogSelection {
    AllNew,
    Ids(Vec<String>),
}

/// Remove the `{id: <id>}` row from `config[key]`; absent is an
/// unknown-resource refusal, not a silent no-op.
fn remove_row(config: &mut Value, key: &str, id: &str) -> Result<()> {
    let rows = config
        .get_mut(key)
        .and_then(Value::as_array_mut)
        .ok_or_else(|| Error::invalid("invalid_config", format!("config.{key} is missing")))?;
    let before = rows.len();
    rows.retain(|row| row.get("id").and_then(Value::as_str) != Some(id));
    if rows.len() == before {
        return Err(Error::invalid(
            "invalid_request",
            format!("unknown {key} id '{id}'"),
        ));
    }
    Ok(())
}
