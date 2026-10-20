//! CAD-1586: `master.status` — the consolidated master diagnosis read
//! (served through `agent_settings_command`, never a free-read RPC),
//! and the shared configuration-unlaunchable send preflight.
//!
//! Status reports, in one document: the registered row and live
//! session fields [`Shared::rpc_master_state`] already derives, the
//! configured model and its recorded selection source, a *fresh*
//! launch check resolved against the stored Pi policy (the same
//! [`crate::pi_policy`] gates `master start` and the adapter's `open`
//! enforce — this module picks no model and keeps no second
//! allowlist), the applicable role allowlist, the pending queue and
//! the newest turn. Provider authentication and endpoint liveness are
//! proven only by launching, so status never claims them: `auth` is
//! always `"unknown"` and a launchable verdict means "the
//! configuration does not refuse", not "a launch will succeed".
//!
//! [`Shared::master_send_preflight`] is the other half: the send
//! funnel calls it before every durable enqueue to the master so a
//! configuration that provably refuses every launch answers
//! `master_unlaunchable` immediately instead of silently queueing a
//! turn no launch can deliver (the CAD-1586 observation). It fires
//! only on a fresh, deterministic configuration verdict while no
//! actor owns the master — a busy, starting or merely offline master
//! is never refused, and queued mail already enqueued is untouched:
//! an authorized repair plus the existing lifecycle resumes it in
//! place, with the unknown-outcome fence unchanged.

use super::*;
use crate::master::{self, ALIAS};
use crate::pi_policy::PiPolicy;

/// `launch.code` for a model with no `provider/id` — the master's own
/// tool-execution classification needs the provider segment
/// (CAD-602); it fails before the allowlist is even consulted.
const CODE_UNQUALIFIED_MODEL: &str = "model_unqualified";

/// Refusal classification the status document and the send preflight
/// share. `kind` separates the master row's own configuration from
/// host capability (Landlock) — only the configuration kind can ever
/// refuse a send.
struct MasterRefusal {
    /// Stable reason code — `launch.code` in status.
    code: &'static str,
    /// `configuration` or `host`.
    kind: &'static str,
    /// The actionable reason text.
    reason: String,
}

impl MasterRefusal {
    fn config(code: &'static str, reason: impl Into<String>) -> Self {
        Self {
            code,
            kind: "configuration",
            reason: reason.into(),
        }
    }

    fn host(code: &'static str, reason: impl Into<String>) -> Self {
        Self {
            code,
            kind: "host",
            reason: reason.into(),
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "ok": false,
            "code": self.code,
            "kind": self.kind,
            "reason": self.reason,
        })
    }
}

/// Re-derive which of `require_allowed`'s ordered gates produced its
/// refusal — the message carries the facts, the classification must
/// name which one failed. The order and the predicates mirror
/// [`crate::pi_policy::require_allowed`] exactly; the cursor provider
/// is always agentic for a master, so `unsafe_transport` is
/// unreachable for this role and any later gate falls to the generic
/// policy code.
fn pi_refusal_code(model: &str, policy: Option<&PiPolicy>) -> &'static str {
    if !model.contains('/') {
        return CODE_UNQUALIFIED_MODEL;
    }
    let provider = model.split_once('/').map(|(p, _)| p).unwrap_or(model);
    let agentic = provider == "cursor"
        || policy.is_some_and(|p| p.agentic_providers.iter().any(|p| p == provider));
    if agentic {
        return "agentic_provider";
    }
    if crate::pi_policy::require_safe_transport(model).is_err() {
        return "unsafe_transport";
    }
    "model_not_allowed"
}

/// The deterministic configuration gate a fresh launch of the
/// *registered* master hits: exactly what `PiAdapter::open` checks
/// for the master before any spawn — the stored `params.model` must
/// exist and satisfy `require_allowed` for role `master` against the
/// stored policy. A non-Pi master carries no model allowlist gate.
/// `None` means configuration does not refuse the launch — never a
/// promise that the launch succeeds (credentials, provider reach and
/// the host stay unproven).
fn registered_config_refusal(
    agent: &store::Agent,
    policy: Option<&PiPolicy>,
) -> Option<MasterRefusal> {
    if !crate::adapter::registry::pi_family_provider(&agent.provider) {
        return None;
    }
    let model = agent
        .params
        .as_ref()
        .and_then(|p| p.get("model"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if model.trim().is_empty() {
        // Same refusal PiAdapter::open raises for a master row with no
        // launch model (CAD-559): a silent provider fallback is never
        // used.
        return Some(MasterRefusal::config(
            "model_missing",
            format!(
                "pi agent '{ALIAS}' has no model configured — the operator's launch \
                 params carry none; set the master default in Settings > Agents; a \
                 silent provider fallback is never used (CAD-559)"
            ),
        ));
    }
    match crate::pi_policy::require_allowed(policy, "master", model) {
        Ok(()) => None,
        Err(e) => Some(MasterRefusal::config(
            pi_refusal_code(model, policy),
            e.to_string(),
        )),
    }
}

/// The applicable role allowlist: the stored policy's `master` list
/// and the pm.yaml key it came from, `applicable: false` for a
/// non-Pi master (no model allowlist governs it).
fn allowlist_json(provider: &str, policy: Option<&PiPolicy>) -> Value {
    if !crate::adapter::registry::pi_family_provider(provider) {
        return json!({"applicable": false});
    }
    let (key, allow): (&str, &[String]) = match policy {
        Some(p) => (p.models.allow_key("master"), p.models.allow_for("master")),
        // An absent `[pi]` denies every model; `require_allowed` names
        // `allow` in that refusal, so status names the same key.
        None => ("allow", &[]),
    };
    json!({
        "applicable": true,
        "key": key,
        "entries": allow,
    })
}

/// `stored_pi_policy` classified for a launch verdict. A rejected or
/// structured read (unimported, invalid or missing authority) is a
/// *deterministic configuration gap*: `PiAdapter::open` hits the same
/// `store.pi_policy()` failure and refuses, so callers classify it as
/// `policy_unavailable`. An internal store error is transient — it
/// propagates instead of producing a false configuration verdict.
/// `needed: false` skips the read for a provider with no policy gate.
fn pi_policy_for(
    needed: bool,
    shared: &Shared,
) -> Result<std::result::Result<Option<PiPolicy>, String>> {
    if !needed {
        return Ok(Ok(None));
    }
    match shared.stored_pi_policy() {
        Ok(policy) => Ok(Ok(policy)),
        Err(Error::Rejected(reason)) => Ok(Err(reason)),
        Err(Error::Structured(structured)) => Ok(Err(structured.message)),
        Err(error) => Err(error),
    }
}

/// The `launch` field: `verdict` from [`registered_config_refusal`] or
/// the `master start` check chain; `scope_ok` names the boundary of
/// what a clean verdict actually proves.
fn launch_json(verdict: Option<MasterRefusal>, scope_ok: &'static str) -> Value {
    match verdict {
        None => json!({
            "ok": true,
            "code": null,
            "kind": null,
            "reason": null,
            "scope": scope_ok,
        }),
        Some(refusal) => refusal.to_json(),
    }
}

const REGISTERED_SCOPE: &str =
    "configuration — the stored launch params re-checked against the stored \
     Pi policy; provider credentials, reachability and host state are not proven";
const UNREGISTERED_SCOPE: &str =
    "the checks a bare `master start` runs — agent files, provider pick, \
     Pi policy, host confinement; provider credentials and reachability \
     are not proven";

impl Shared {
    /// CAD-1586 send preflight: refuse a fresh send to a registered,
    /// currently unowned master whose own stored launch configuration
    /// provably refuses every launch — `master_unlaunchable` (`gate`)
    /// before the durable enqueue, identical on every send entry
    /// (`agent_send`, `send`/`ask`, `thread_send`, `cli_message_send`,
    /// dispatches) because the callers share this funnel.
    ///
    /// Only a *configuration* verdict refuses, and only while no actor
    /// owns the master: a live, starting or stopping master queues
    /// normally, an offline-but-launchable one keeps accepting queued
    /// work for its next start, and an unregistered master is already
    /// refused by the enqueue itself. Passing is not a launch
    /// guarantee — credentials and the host are still unproven here.
    pub(super) fn master_send_preflight(&self, alias: &str) -> Result<()> {
        if !master::is_master(alias) {
            return Ok(());
        }
        if self.lifecycle.lock().unwrap().owned(alias) {
            return Ok(());
        }
        let Some(agent) = self.store.agent_opt(alias)? else {
            return Ok(());
        };
        let verdict = match pi_policy_for(
            crate::adapter::registry::pi_family_provider(&agent.provider),
            self,
        )? {
            Ok(policy) => registered_config_refusal(&agent, policy.as_ref()),
            Err(reason) => Some(MasterRefusal::config("policy_unavailable", reason)),
        };
        match verdict {
            None => Ok(()),
            Some(refusal) => Err(crate::error::Error::gate_coded(
                "master_unlaunchable",
                format!(
                    "the master cannot launch on its stored configuration: {} — \
                     `cadence master status` reports the full diagnosis; repair it \
                     in Settings > Agents and start the master again. The message \
                     was not queued; already-queued mail is untouched (CAD-1586)",
                    refusal.reason
                ),
            )),
        }
    }

    /// `master.status` — one consolidated diagnosis for the master:
    /// registered row and live-session facts from
    /// [`Self::rpc_master_state`], the configured launch model and its
    /// recorded selection source, a fresh policy launch check, the
    /// applicable allowlist, the fence flag, the pending count and the
    /// newest turn. Read-only like `master_state`; every piece is
    /// derived, nothing is written. Unprovable facts report
    /// `"unknown"`, never a guess: `auth` cannot be decided without a
    /// launch, and a clean launch check is not a launch promise.
    ///
    /// This is the bare document — it answers to the `agent_settings_command`
    /// `master.status` operation, whose caller gate (operator or a
    /// settings grant bound to this exact operation + payload + requester
    /// identity) is the only way in. It is deliberately NOT a free
    /// Rule::Read RPC: the allowlist and launch configuration it reports
    /// are settings facts an ordinary worker is not entitled to see.
    pub(super) fn master_status_document(&self) -> Result<Value> {
        match self.store.agent_opt(ALIAS)? {
            Some(agent) => self.registered_status(&agent),
            None => self.unregistered_status(),
        }
    }

    /// `master.resume` — the designated-agent recovery arm of
    /// `agent_settings_command`: bring the registered master back after
    /// its configuration is repaired in Settings > Agents, while the
    /// operator is away (CAD-1586).
    ///
    /// The target is fixed to the canonical master alias — there is no
    /// caller-supplied alias to bind a grant against, so `check_payload`
    /// admits only `grant_id`/`payload_sha256` and the approved payload
    /// is exactly `{}`. Authority is identical to every other settings
    /// op: the proven operator runs it free; any agent connection needs
    /// a settings grant bound to this operation + subject + `{}` digest.
    /// The op is write-class (`settings.write` scope for a remote peer)
    /// because it starts a process, but it is not revision-bound: it
    /// never touches the settings document CAS.
    ///
    /// This is [`Shared::try_resume`] verbatim — the same function
    /// `agent_resume` calls after its own caller rule — so every
    /// safeguard is inherited, not reimplemented: a live master is
    /// "already live", an in-flight start/stop is "retry", an
    /// unreconciled unknown-outcome fence REFUSES (resume is the wrong
    /// repair for a fence — the operator reconciles), and a false
    /// return reports `attention` rather than claiming a start. It is
    /// not a second launcher: unregistered master is refused by the
    /// store read; `master start` stays the operator's verb.
    pub(super) fn master_resume(self: &Arc<Self>) -> Result<Value> {
        let started = self.try_resume(ALIAS)?;
        let state = if started { "starting" } else { "attention" };
        Ok(json!({"alias": ALIAS, "state": state}))
    }

    /// The registered row: stored facts plus the live session read of
    /// `master_state` (its `model`/`turn`/`queued`/`live` fields are
    /// reused verbatim — the same derivation, never a second one).
    fn registered_status(&self, agent: &store::Agent) -> Result<Value> {
        // `master_state` refuses only an absent row; the row exists.
        let base = self.rpc_master_state(&Value::Null)?;
        let policy = pi_policy_for(
            crate::adapter::registry::pi_family_provider(&agent.provider),
            self,
        )?;
        let launch = match &policy {
            Ok(policy) => registered_config_refusal(agent, policy.as_ref()),
            Err(reason) => Some(MasterRefusal::config("policy_unavailable", reason.clone())),
        };
        let allowlist_error = policy.as_ref().err().cloned();
        let selection = agent.model_selection.clone().unwrap_or_else(|| {
            crate::model_defaults::legacy_selection(
                registry::supports_model(&agent.provider, &agent.endpoint_kind),
                agent.team_role.as_deref(),
                &agent.role,
                agent
                    .params
                    .as_ref()
                    .and_then(|p| p.get("model"))
                    .and_then(Value::as_str),
            )
        });
        // The newest message row — live or terminal — is the last turn
        // the mailbox saw. One index row, never a body read.
        let last = self
            .store
            .message_index(
                ALIAS,
                &store::MessageIndexQuery {
                    states: &[],
                    from: None,
                    source: None,
                    since: None,
                    limit: 1,
                    include_app: true,
                },
            )?
            .rows
            .last()
            .map(store::MessageIndex::to_json)
            .unwrap_or(Value::Null);
        let mut out = base;
        out["registered"] = json!(true);
        out["configured"] = json!({
            "model": agent
                .params
                .as_ref()
                .and_then(|p| p.get("model"))
                .and_then(Value::as_str),
            "effort": agent
                .params
                .as_ref()
                .and_then(|p| p.get("effort"))
                .and_then(Value::as_str),
            "source": selection.get("source").cloned().unwrap_or(Value::Null),
            "selection": selection,
        });
        out["launch"] = launch_json(launch, REGISTERED_SCOPE);
        let mut allowlist = allowlist_json(
            &agent.provider,
            policy.as_ref().ok().and_then(|p| p.as_ref()),
        );
        if let Some(error) = allowlist_error {
            allowlist["error"] = json!(error);
        }
        out["allowlist"] = allowlist;
        out["confined"] = json!(master::is_confined(
            agent.params.as_ref(),
            master::confinement_available(&self.provider_env).is_ok(),
        ));
        out["enabled"] = json!(agent.enabled);
        out["error"] = json!(agent.error.as_deref());
        out["fence"] = json!({"unknown_outcome": self.store.has_unknown(ALIAS)?});
        out["queue"] = json!({
            "pending": out["queued"].clone(),
            "last": last,
        });
        out["auth"] = json!("unknown");
        Ok(out)
    }

    /// No master row: what a bare `master start` would resolve and
    /// where it stops — the same check order `rpc_master_start`
    /// applies (tracker files, provider pick, Pi policy resolution,
    /// host confinement). Read-only: nothing is installed, recorded
    /// or registered, and a digest drift is *reported* against the
    /// recorded baseline without recording anything.
    fn unregistered_status(&self) -> Result<Value> {
        // The files `master start` would brief from: each present file
        // plus the shipped template for a missing one, so the provider
        // pick mirrors what `install_defaults` leaves behind. The
        // read/verify checks are the same gates — a refusal is
        // reported, not raised.
        let mut refusal: Option<MasterRefusal> = None;
        let mut agent_md = String::new();
        let mut missing: Vec<String> = Vec::new();
        let files = match self
            .pm_dir()
            .and_then(|pm_dir| master::read_files(&pm_dir, ALIAS, false))
        {
            Ok(files) => files,
            Err(e) => {
                refusal = Some(MasterRefusal::config(
                    "files_unreadable",
                    format!(
                        "{e} — `master start` refuses before it installs or \
                         registers anything"
                    ),
                ));
                Vec::new()
            }
        };
        for (name, text) in &files {
            if name == "AGENT.md" {
                agent_md = text.clone();
            }
        }
        for name in master::FILES {
            if files.iter().all(|(n, _)| n.as_str() != name) {
                missing.push(name.to_string());
                if name == "AGENT.md" {
                    if let Some(template) = master::default_template(ALIAS, name) {
                        agent_md = template.to_string();
                    }
                }
            }
        }
        if refusal.is_none() {
            if let Some(changed) = master::tampered_file(&self.state_dir, ALIAS, &files) {
                refusal = Some(MasterRefusal::config("files_tampered", changed));
            }
        }
        // The provider pick is `master start`'s own resolution — an
        // explicit request is absent here, so AGENT.md's preferred
        // (then fallbacks, then the first accepted provider) decides.
        let prefs = super::master_rpc::preferred(&agent_md);
        let provider = super::master_rpc::resolve_provider(None, &prefs);
        let choice = prefs.iter().find(|c| c.0 == provider);
        let mut configured_model: Option<String> = choice.and_then(|c| c.1.clone());
        let mut model_source: Option<&str> = configured_model.as_ref().map(|_| "agent_md");
        // The policy read happens once, only for a pi pick, and a
        // rejected/structured read is a configuration verdict (the same
        // read `master start` and the adapter's `open` fail on).
        let policy = pi_policy_for(
            crate::adapter::registry::pi_family_provider(&provider),
            self,
        )?;
        let policy_ref = policy.as_ref().ok().and_then(|p| p.as_ref());
        let allowlist_error = policy.as_ref().err().cloned();
        if refusal.is_none() && crate::adapter::registry::pi_family_provider(&provider) {
            refusal = match &policy {
                Err(reason) => Some(MasterRefusal::config("policy_unavailable", reason.clone())),
                Ok(_) => match crate::pi_policy::resolve_model_with_source(
                    policy_ref,
                    "master",
                    configured_model.as_deref(),
                ) {
                    Ok((resolved, platform)) => {
                        model_source = Some(if platform {
                            crate::model_defaults::SOURCE_PLATFORM_DEFAULT
                        } else if configured_model.is_none() {
                            crate::model_defaults::SOURCE_PI_POLICY_DEFAULT
                        } else {
                            "agent_md"
                        });
                        configured_model = Some(resolved);
                        None
                    }
                    Err(e) => {
                        // The resolution errs only on a configured
                        // model — AGENT.md's or the policy's own role
                        // default (the platform fallback never refuses).
                        let model = configured_model
                            .as_deref()
                            .or_else(|| {
                                policy_ref.and_then(|p| p.models.default.for_role("master"))
                            })
                            .unwrap_or_default();
                        Some(MasterRefusal::config(
                            pi_refusal_code(model, policy_ref),
                            e.to_string(),
                        ))
                    }
                },
            };
        }
        if refusal.is_none() {
            if let Err(e) = master::confinement_available(&self.provider_env) {
                refusal = Some(MasterRefusal::host(
                    "confinement_unavailable",
                    format!(
                        "{e}. The master cannot be confined on this host — `master start` \
                         refuses unless the operator passes --unconfined"
                    ),
                ));
            }
        }
        let mut allowlist = allowlist_json(&provider, policy_ref);
        if let Some(error) = allowlist_error {
            allowlist["error"] = json!(error);
        }
        let mut out = json!({
            "alias": ALIAS,
            "registered": false,
            "live": false,
            "state": "unregistered",
            "provider": provider,
            "endpoint_kind": "managed",
            "configured": {
                "model": configured_model,
                "effort": choice.and_then(|c| c.2.clone()),
                "source": model_source,
            },
            "launch": launch_json(refusal, UNREGISTERED_SCOPE),
            "allowlist": allowlist,
            "queue": {"pending": 0, "last": null},
            "auth": "unknown",
            "files": {"missing": missing},
        });
        out["note"] = json!(
            "no master is registered — `launch` is what a bare `master start` \
             would resolve and where it would refuse; nothing was installed"
        );
        Ok(out)
    }
}
