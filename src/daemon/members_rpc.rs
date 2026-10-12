//! CAD-1380/AOS-216: `member_*` daemon RPC handlers — the member plane the
//! board's `/api/assist/<verb>` route relays to.
//!
//! A member never reaches the daemon's Unix socket and never carries an
//! agent/operator caller identity. The member proof is the **signed
//! assistant envelope itself** (`member_envelope`): the `/api/assist/*`
//! route verifies it, consumes its single-use `jti` at the HTTP boundary
//! (the ONE defined replay-consumption point), and forwards the raw signed
//! bytes to the daemon; the daemon then **re-verifies the Ed25519
//! signature and every claim pin against its own `board-identity.json` +
//! the platform JWKS** — daemon-verifiable authority, so the board relay
//! (or any caller) can never widen member authority by asserting a
//! `member_as`-style field it merely claims. The daemon does NOT re-consume
//! the jti (the route is the single consumption point); it re-checks the
//! cryptography only.
//!
//! `member_envelope` is honored only on a daemon-proven operator/board
//! connection — the channel the board's relay runs on. An agent
//! connection, an unproven connection, or any caller presenting a
//! `member_envelope` is refused at `proven_operator` before the envelope
//! is even read. After cryptographic re-verify the daemon re-binds the
//! envelope's `(org, sub, assistant)` to the stored `assistants` row it
//! owns (created by daemon-side provisioning, never by request), refusing
//! a triple naming no binding, a different org, or a non-`active` state.
//!
//! Every member effect re-checks the stored binding's `state` — `active`
//! admits, `revoked`/`archived` refuse. The live platform-membership
//! check happens on the board side at mint + at every `/call`
//! (`resolveWikiActor` re-reads `workspace_members`); a removed member
//! cannot mint or call. The deferred-effect commit-time revalidation
//! (kickoff §4) the platform's `POST /__platform/assistant/revalidate`
//! supplies is NOT yet invoked from a daemon commit path — see the
//! "Open gaps" note in the interface doc; no revocation acceptance is
//! claimed for a queued/resumed effect beyond the stored-binding refuse.

use std::io::Read as _;

use super::*;

use crate::board_identity;

/// Refuse any request field outside `allowed` — the member surface admits
/// only `member_envelope` plus the verb's own arguments; an unlisted or
/// identity field is never silently dropped.
fn reject_unknown_fields(params: &Value, allowed: &[&str], verb: &str) -> Result<()> {
    let object = params
        .as_object()
        .ok_or_else(|| Error::rejected(format!("{verb} takes an object")))?;
    if let Some(field) = object.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(Error::rejected(format!(
            "{verb} takes {} only; field '{field}' is not accepted",
            allowed.join(", ")
        )));
    }
    Ok(())
}

/// A daemon-proven member caller — bound to a stored assistant row AND
/// re-verified against the signed envelope the board relayed.
pub(crate) struct MemberCaller {
    pub org: String,
    pub sub: String,
    pub assistant_id: String,
    /// The daemon-owned agent alias the assistant runs as.
    pub alias: String,
    pub conversation_id: Option<String>,
    /// Binding state at admission (re-checked every effect; `active` only).
    #[allow(dead_code)]
    pub state: String,
    /// The envelope's issuing credential — the member's recorded authority
    /// provenance a deferred commit re-proves.
    pub credential_id: String,
}

/// The operation + scope + (optional) job + op provenance a
/// `member_authority_live` re-prove carries — the strict revalidate
/// body's bound fields. `op_credential_id` is the queued effect's own
/// recorded `assist_requests.credential_id` — required for
/// `job_dispatch`, absent for every assistant-resource verb.
pub(super) struct MemberReprove<'a> {
    pub operation: &'a str,
    pub scope: &'a str,
    pub job_id: Option<&'a str>,
    /// The queued effect's own recorded provenance — required for
    /// `job_dispatch`, absent for every other op. The platform re-proves
    /// THIS credential live, never the binding's latest pin.
    pub op_credential_id: Option<&'a str>,
}

/// Daemon-side JWKS cache for the assist family — independent of the HTTP
/// route's, so the daemon's re-verify does not depend on the route's cache.
static MEMBER_JWKS: Mutex<crate::board_identity::JwksCache> =
    Mutex::new(crate::board_identity::JwksCache::new());

impl Shared {
    /// CAD-1380: a member-bound personal assistant (`assist-*` alias)
    /// reaches only the member-assistant verb surface — its own
    /// conversation/thread reads, its own message report, the job
    /// records bound to it, and the assist plumbing. Every generic
    /// worker/operator verb — other agents, issues, wiki, dispatch,
    /// register, operator effects — is refused here, before the method
    /// runs, by the derived caller's alias class. This is the
    /// member-boundary complement to `master_policy`: it scopes the
    /// member's own runtime (a worker agent), never widens it.
    ///
    /// The check reads only the derived caller identity — never a
    /// request field. A connection that derives no agent identity
    /// (operator, unproven) is untouched: it takes its own rule.
    pub(super) fn member_assist_policy(
        &self,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<()> {
        // `member_*` verbs themselves are gated by `member_caller` (the
        // proven relay + signed envelope), not by this agent-class gate —
        // an assist alias never calls them itself.
        if method.starts_with("member_") {
            return Ok(());
        }
        let alias = match self.caller_identity(peer_pid) {
            Ok(Caller::Agent(v)) => v.agent.alias.clone(),
            _ => return Ok(()),
        };
        // The member boundary is SOURCE-VERIFIED and fail-CLOSED: the
        // caller is a member assistant iff the `assistants` table holds
        // a binding for this alias — the immutable (org, sub) record,
        // never a request field or a name prefix. A revoked/archived
        // binding KEEPS the alias inside the member boundary (it is
        // still a member assistant, just a dead one — the per-resource
        // checks below still refuse); it does NOT drop to generic
        // worker rules. A store read failure here is an error, not an
        // "unbound" answer — the member boundary never falls open on an
        // unreadable binding table.
        let binding = match self.store.assistant_for_alias(&alias) {
            Ok(Some(b)) => b,
            Ok(None) => return Ok(()), // not a member assistant — ordinary worker/operator rule
            Err(e) => {
                return Err(Error::rejected(format!(
                    "member assist refused: the assistant binding for '{alias}' could not be \
                     read ({e}) — the member boundary fails closed"
                )));
            }
        };
        // The member-assistant verb surface — a bounded whitelist, each
        // entry carrying the per-resource check its handler needs. A
        // method absent here is refused before it runs; a method
        // present but naming a resource outside the member's own
        // surface is refused by the per-verb arm below.
        match method {
            // Own alias only: the agent reads/steers/interrupts itself.
            "agent_show" | "agent_events" | "agent_send" | "agent_ask" => {
                let target = params
                    .get("alias")
                    .or_else(|| params.get("to"))
                    .or_else(|| params.get("agent"))
                    .and_then(Value::as_str);
                // A missing target means the handler resolves to the
                // caller's own alias; a present one must equal it.
                if let Some(t) = target {
                    if t != alias {
                        return Err(Error::rejected(format!(
                            "assist agent '{alias}' may not {method} '{t}' — only its own \
                             principal surface (CAD-1380)"
                        )));
                    }
                }
            }
            // Own conversation thread only.
            "thread_read" => {
                let target = params
                    .get("alias")
                    .and_then(Value::as_str)
                    .unwrap_or(alias.as_str());
                if target != alias {
                    return Err(Error::rejected(format!(
                        "assist agent '{alias}' may not thread_read '{target}' — only its own \
                         conversation (CAD-1380)"
                    )));
                }
            }
            // Own messages only — `message_read`/`message_report` name a
            // message id; bound to this alias's message row.
            "message_read" | "message_report" => {
                let id = params
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let own = self
                    .store
                    .message(id)
                    .ok()
                    .flatten()
                    .map(|m| m.alias == alias)
                    .unwrap_or(false);
                if !own {
                    return Err(Error::rejected(format!(
                        "assist agent '{alias}' may not {method} '{id}' — the message is not \
                         its own (CAD-1380)"
                    )));
                }
            }
            // Own bound jobs only: `job_show`/`job_events` name a job
            // this member's assistant owns or subscribes.
            "job_show" | "job_events" => {
                let job = params
                    .get("job")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let owns = self
                    .store
                    .assist_request_owns(&binding.org, &binding.sub, job)
                    .unwrap_or(false);
                let subscribes = self
                    .store
                    .job_is_subscriber(job, &binding.org, &binding.sub)
                    .unwrap_or(false);
                if !owns && !subscribes {
                    return Err(Error::rejected(format!(
                        "assist agent '{alias}' may not {method} '{job}' — the job is not \
                         bound to its principal (CAD-1380)"
                    )));
                }
            }
            // A task read is scoped through its job's ownership.
            "task_show" => {
                let task_id = params
                    .get("task")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let job_id = self
                    .store
                    .task(task_id)
                    .ok()
                    .map(|t| t.job_id)
                    .unwrap_or_default();
                let owns = self
                    .store
                    .assist_request_owns(&binding.org, &binding.sub, &job_id)
                    .unwrap_or(false);
                let subscribes = self
                    .store
                    .job_is_subscriber(&job_id, &binding.org, &binding.sub)
                    .unwrap_or(false);
                if !owns && !subscribes {
                    return Err(Error::rejected(format!(
                        "assist agent '{alias}' may not task_show '{task_id}' — the task is \
                         not bound to its principal (CAD-1380)"
                    )));
                }
            }
            // Interrupt: own live turn only — the alias it names must be
            // itself and a turn must actually be running.
            "interrupt" => {
                let target = params
                    .get("alias")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let running = self.store.running_message(&alias).ok().flatten().is_some();
                if target != alias || !running {
                    return Err(Error::rejected(format!(
                        "assist agent '{alias}' may not interrupt '{target}' — only its own \
                         live turn (CAD-1380)"
                    )));
                }
            }
            // Every other method — including broad reads like `health`,
            // `daemon_info`, `agent_list`, `job_list`, issues, wiki,
            // dispatch, register, plans and every operator effect — is
            // refused for a member assistant. Its surface is its own
            // conversation, its own mail and the jobs bound to it.
            _ => {
                return Err(Error::rejected(format!(
                    "assist agent '{alias}' may not call '{method}' — its surface is \
                     its own conversation and the jobs bound to it; fleet/issue/wiki/\
                     dispatch/operator effects are refused (member boundary, CAD-1380)"
                )));
            }
        }
        Ok(())
    }

    /// Re-verify the member's signed envelope at the daemon. The config is
    /// the daemon's own `board-identity.json` (issuer/audience/company the
    /// operator configured), never a request field. Returns the verified
    /// actor's `(org, sub, assistant_id, scopes)` — the scopes come from
    /// the verified claims and feed the daemon-side per-verb scope check.
    fn member_envelope_actor(
        &self,
        envelope: &str,
        verb: &str,
    ) -> Result<(String, String, String, Vec<String>, String)> {
        let config = board_identity::read_config(&self.state_dir).map_err(|_| {
            Error::rejected(format!(
                "{verb} refused: no board identity is configured to verify member authority"
            ))
        })?;
        let kid = crate::assistant_actor::peek_kid(envelope).ok_or_else(|| {
            Error::rejected(format!(
                "{verb} refused: member envelope header is not the shape"
            ))
        })?;
        let now = crate::issue::time::now_epoch();
        let key = {
            let mut cache = MEMBER_JWKS.lock().unwrap_or_else(|e| e.into_inner());
            cache.key(&config.issuer, &kid, now).map_err(|e| {
                Error::rejected(format!(
                    "{verb} refused: member envelope key check failed: {e}"
                ))
            })?
        };
        let actor = crate::assistant_actor::verify(envelope, &config, &key, now).map_err(|r| {
            Error::rejected(format!("{verb} refused: member envelope: {}", r.message))
        })?;
        Ok((
            actor.org,
            actor.sub,
            actor.assistant,
            actor.scopes,
            actor.credential_id,
        ))
    }

    /// LIVE-PULL authority revalidation (kickoff §4 — the contract
    /// requirement that a ≤300 s JWT, a provisioning-time check, or a
    /// locally-cached/stored binding is NOT revocation proof). At every
    /// protected member read/effect/resume the daemon re-asks the
    /// platform whether the member is STILL live, over the board's own
    /// `POST https://{host}/__platform/assistant/revalidate`, presenting
    /// the member's signed envelope as the bearer (the member re-proves
    /// their own `(org, sub, assistant)` — no service credential, no
    /// repurposed `results.submit` enrollment, no user impersonation) and
    /// the `{organization_id, sub, assistant_id}` triple. The platform
    /// re-reads `workspace_members` + `personalAssistants` live and
    /// answers `{allow: bool}`.
    ///
    /// FAIL CLOSED: an unreachable board, a non-200, a malformed body, or
    /// `allow:false` all refuse — a removed member's envelope still
    /// verifies (the signature is valid) but `revalidate` answers
    /// `allow:false` because the live membership row is gone.
    ///
    /// `state_dir` is the daemon's own; `board_identity::Config` supplies
    /// the board's public `host`. The pull is bounded (small timeout,
    /// bounded body, https-for-public-host) and never logs the envelope.
    pub(super) fn member_authority_live(
        &self,
        envelope: &str,
        member: &MemberCaller,
        revalidate: MemberReprove,
        verb: &str,
    ) -> Result<()> {
        let MemberReprove {
            operation,
            scope,
            job_id,
            op_credential_id,
        } = revalidate;
        let org = member.org.as_str();
        let sub = member.sub.as_str();
        let assistant_id = member.assistant_id.as_str();
        let config = board_identity::read_config(&self.state_dir).map_err(|_| {
            Error::rejected(format!(
                "{verb} refused: no board identity configured for member authority revalidation"
            ))
        })?;
        // The board's public host — where the `__platform/*` worker serves.
        // `public_scheme` mirrors the container route's aud derivation.
        let host = config.host.trim_end_matches('/');
        let scheme = {
            let name = host.split(':').next().unwrap_or_default();
            if name == "localhost" || name.ends_with(".localhost") {
                "http"
            } else {
                "https"
            }
        };
        let url = format!("{scheme}://{host}/__platform/assistant/revalidate");
        let agent_cfg = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(8)))
            .http_status_as_error(false)
            .max_redirects(0)
            .build();
        let agent = ureq::Agent::new_with_config(agent_cfg);
        // The daemon binds the operation + its required scope to the
        // protected verb itself — never caller-supplied — so the platform
        // re-derives `allow` from the member's CURRENT role, the live
        // issuing-credential state and grant ceiling, and the active
        // assistant binding. `resource` is server-bound to the member's
        // own assistant (`kind:"assistant"`).
        let mut resource = serde_json::Map::new();
        resource.insert("kind".to_string(), json!("assistant"));
        // Job-bound verbs propagate the actual job id so the platform
        // re-checks the resource the effect names, not a generic
        // assistant resource. `None` for the others.
        if let Some(job_id) = job_id {
            resource.insert("job_id".to_string(), json!(job_id));
        }
        // `job_dispatch` carries the queued request's own
        // `assist_requests.credential_id` — the platform re-proves
        // THIS recorded credential live, never the binding's latest pin,
        // so a stale queued effect denied after revocation does NOT
        // ride a renewal reconcile. `None` for every other op.
        if let Some(credential_id) = op_credential_id {
            resource.insert("credential_id".to_string(), json!(credential_id));
        }
        let body = serde_json::json!({
            "version": "hosted-cadence-assistant.v1",
            "organization_id": org,
            "sub": sub,
            "assistant_id": assistant_id,
            "operation": operation,
            "scope": scope,
            "resource": resource,
        });
        // Bearer: prefer the daemon's own enrolled `hosted-board` service
        // credential (least-authority `assist.revalidate` deferred-commit
        // authority, operator-enrolled via `platform_enroll`); fall back to
        // the member's signed envelope for member-initiated re-proving.
        // FAIL-CLOSED on custody: when a `hosted-board` record EXISTS but
        // its custody bytes cannot be loaded/validated, the request
        // refuses — a configured-but-unloadable service binding must never
        // silently degrade to the member path (no custody-corruption
        // fallback). Only a genuinely ABSENT record uses the member
        // envelope path.
        let bearer: String;
        match self.store.platform_credential("hosted-board", host) {
            Err(e) => {
                return Err(Error::rejected(format!(
                    "{verb} refused: the hosted-board service credential record \
                     could not be read: {e}"
                )));
            }
            Ok(Some(record)) => {
                let key = crate::platform::custody::Key {
                    platform: "hosted-board",
                    account: host,
                };
                let token = self
                    .platform_custody
                    .load(&record.custody, &key)
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty());
                match token {
                    Some(t) => bearer = t,
                    None => {
                        return Err(Error::rejected(format!(
                            "{verb} refused: the configured hosted-board service \
                             credential has no loadable custody material"
                        )));
                    }
                }
            }
            Ok(None) => bearer = format!("wikienv_{envelope}"),
        }
        let mut resp = agent
            .post(&url)
            .header("content-type", "application/json")
            // Bearer selection — the daemon prefers its OWN dedicated
            // `assist.revalidate` board-service credential when the
            // operator has enrolled one into `platform_credentials`
            // custody under `platform="hosted-board"` (the least-authority
            // deferred-commit path — a stored job resuming with no fresh
            // member envelope). Absent that record, the member's signed
            // envelope is the bearer (member-initiated re-proving). Both
            // land `allow` only when the TARGET member's live role +
            // live issuing-credential + active binding all still hold.
            .header("authorization", &format!("Bearer {}", bearer))
            .send_json(body)
            .map_err(|e| {
                Error::rejected(format!(
                    "{verb} refused: member authority revalidation unreachable: {e}"
                ))
            })?;
        if resp.status() != 200 {
            return Err(Error::rejected(format!(
                "{verb} refused: member authority revalidation answered HTTP {}",
                resp.status().as_u16()
            )));
        }
        let mut bytes = Vec::new();
        resp.body_mut()
            .as_reader()
            .take(64 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|e| {
                Error::rejected(format!("{verb} refused: revalidation body unreadable: {e}"))
            })?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
            Error::rejected(format!(
                "{verb} refused: revalidation body is not the contract shape"
            ))
        })?;
        if value.get("allow").and_then(Value::as_bool) != Some(true) {
            return Err(Error::rejected(format!(
                "{verb} refused: member authority is not live at commit — \
                 the platform revalidation did not allow"
            )));
        }
        Ok(())
    }

    /// The member caller: a signed `member_envelope` re-verified at the
    /// daemon — the kickoff's "daemon-verifiable authority" rule. The
    /// member proof is the JWS itself, not a relayed field: the daemon
    /// re-checks the signature and every `iss`/`aud`/`org`/`actor`/
    /// `assistant`/`kind`/`scope` pin itself, so a forged or widened field
    /// can never mint or widen a member identity.
    ///
    /// Admission requires (a) a proven operator/board connection — the
    /// only channel allowed to carry a relayed member envelope — and (b) a
    /// member envelope that re-verifies cryptographically and re-binds to
    /// an `active` stored `assistants` row. The relayed envelope is the
    /// proof; the connection proves only that a trusted relay carried it.
    /// The bound alias is the STORED `assistants.alias` — mint-derived
    /// and member-bound, never a request field.
    ///
    /// Ensure the member's assistant exists as a REAL registered agent
    /// plus a bound private conversation. Idempotent: an existing active
    /// agent + thread is reused; a first provision registers a confined,
    /// read-only pi worker under `assist-<sub>` (a worker profile, never
    /// master, never operator authority) and binds its private home
    /// thread as the member's conversation. The daemon owns every field —
    /// nothing here reads a caller-supplied alias, install or authority.
    ///
    /// The registration records `params.confine=true` + `params.assist` so
    /// the launcher encloses it under the worker confinement policy and
    /// downstream surfaces can tell it is a member assistant, not an
    /// ordinary worker — the assistant never inherits the Master's global
    /// tool allowlist (that is profile-keyed on the master alias).
    fn ensure_assistant_runtime(
        &self,
        binding: &crate::store::AssistantBinding,
    ) -> Result<crate::store::AssistantBinding> {
        // Register the assistant agent lazily. Its cwd is a DEDICATED
        // principal-owned subdirectory — `state_dir/assistants/<sub>`,
        // never the daemon state root — so a confined read-only worker
        // reads/writes only inside its own principal's subtree. `sub` is
        // server-verified and sanitized into the dir name (same charset
        // as the alias); a member can never name a path outside it.
        if self.store.agent_opt(&binding.alias)?.is_none() {
            // The registered alias is the STORED binding alias — derived
            // at mint from the member, never a caller field. The agent-
            // class gate (`member_assist_policy`) keys on that alias
            // family, so a registered member assistant is always caught
            // by the member boundary.
            let sub_safe: String = binding
                .sub
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '-'
                    }
                })
                .take(48)
                .collect();
            let cwd = self.state_dir.join("assistants").join(&sub_safe);
            std::fs::create_dir_all(&cwd).map_err(|e| {
                Error::rejected(format!(
                    "member assist refused: the assistant's principal cwd cannot be created: {e}"
                ))
            })?;
            let params = json!({"confine": true, "assist": binding.sub});
            self.store.register_agent(&crate::store::NewAgent {
                alias: &binding.alias,
                provider: "pi",
                endpoint_kind: "managed",
                role: "worker",
                cwd: cwd.to_str().unwrap_or_default(),
                sandbox: "read-only",
                instructions: Some(
                    "You are a member's personal assistant bound to ONE principal. Read, \
                     research and explain THAT member's authorized work; propose jobs for \
                     the operator/PM to approve. You cannot approve, dispatch, merge, or \
                     touch any other member's state, credentials or files.",
                ),
                params: Some(&params.to_string()),
                team_role: None,
                model_policy: None,
            })?;
        }
        // Bind the private conversation: the alias's home thread is the
        // member's own principal-private conversation. Never a shared app
        // installation or a cloned `install_id` — that would leak other
        // principals' context.
        if binding.conversation_id.is_none() {
            let thread = self.store.ensure_thread(&binding.alias)?;
            self.store
                .assistant_set_conversation(&binding.org, &binding.sub, &thread.id)?;
            return self
                .store
                .assistant_binding(&binding.org, &binding.sub)?
                .ok_or_else(|| Error::rejected("assistant binding vanished after provisioning"));
        }
        Ok(binding.clone())
    }

    /// `member_caller` — admission for every `member_*` verb: proven
    /// operator relay, signed member envelope re-verify, per-verb scope
    /// gate, live binding re-bind, and the §4 live-authority pull bound
    /// to `operation` + its required scope.
    /// `operation`/`need` are the server-bound operation + scope the verb
    /// requires — passed to `member_authority_live` so the platform
    /// re-derives `allow` from CURRENT role + live issuing-credential
    /// state + grant ceiling + active binding, not just membership.
    pub(super) fn member_caller(
        &self,
        params: &Value,
        peer_pid: u32,
        verb: &str,
        operation: &str,
        need: crate::assistant_actor::AssistScope,
        job_id: Option<&str>,
    ) -> Result<MemberCaller> {
        // The rollout gate (CON-002): the platform injects
        // `HOSTED_CADENCE_ASSISTANT_ENABLED=true` into the container env
        // only when the feature is enabled — absent/false/unknown is
        // OFF. Checked BEFORE the envelope is read: a stale envelope, a
        // cached service bearer or a replayed member op can never reach
        // a member effect while the feature is off. The live platform
        // pull below is the authoritative second check.
        if std::env::var("HOSTED_CADENCE_ASSISTANT_ENABLED").as_deref() != Ok("true") {
            return Err(Error::rejected(format!(
                "{verb} refused: member assistant integration is not enabled"
            )));
        }
        // The connection itself must be the operator/board relay — the
        // only channel allowed to carry a relayed member envelope. An
        // agent or unproven connection is refused outright, before the
        // envelope is read.
        self.proven_operator(verb, peer_pid)?;
        let envelope = optional_str(params, "member_envelope").ok_or_else(|| {
            Error::rejected(format!(
                "{verb} refused: no member_envelope proof — \
                 a member operation requires a verified assistant envelope"
            ))
        })?;
        let (org, sub, assistant_id, scopes, credential_id) =
            self.member_envelope_actor(envelope, verb)?;
        // Per-verb scope gate BEFORE the live pull — an `assist.read`-
        // only envelope never reaches the commit-path authority check for
        // a write/jobs verb.
        let member_scopes = scopes.clone();
        let want_scope = match need {
            crate::assistant_actor::AssistScope::Read => "assist.read",
            crate::assistant_actor::AssistScope::Write => "assist.write",
            crate::assistant_actor::AssistScope::Jobs => "assist.jobs",
        };
        if !member_scopes.iter().any(|s| s == want_scope) {
            return Err(Error::rejected(format!(
                "{verb} refused: the member envelope's scopes do not cover {want_scope}"
            )));
        }
        let binding = self.store.assistant_binding(&org, &sub)?.ok_or_else(|| {
            Error::rejected(format!(
                "{verb} refused: no assistant binding for this member — \
                 provisioning happens through the platform mint, not a request"
            ))
        })?;
        if binding.assistant_id != assistant_id {
            return Err(Error::rejected(format!(
                "{verb} refused: member envelope names assistant '{assistant_id}', not the \
                 member's bound assistant '{}'",
                binding.assistant_id
            )));
        }
        if binding.org != org {
            return Err(Error::rejected(format!(
                "{verb} refused: member envelope org '{org}' does not match the binding"
            )));
        }
        if !binding.is_active() {
            return Err(Error::rejected(format!(
                "{verb} refused: the member's assistant binding is {} — revoked/\
                 archived membership admits no effect",
                binding.state
            )));
        }
        // §4 live pull: re-prove the member's platform authority at THIS
        // effect/commit, bound to the exact `operation`+`scope` — the
        // stored binding and the ≤300 s envelope are not revocation proof,
        // and the platform re-derives `allow` from CURRENT role + live
        // issuing-credential state + grant ceiling + active binding, not
        // just membership existence.
        let member_call = MemberCaller {
            org,
            sub,
            assistant_id,
            alias: binding.alias.clone(),
            conversation_id: binding.conversation_id.clone(),
            state: binding.state.clone(),
            credential_id,
        };
        // Member-initiated ops re-prove under the member's own envelope —
        // no `assist_requests` op provenance applies, so `job_dispatch`-
        // style `resource.credential_id` is absent (`None`).
        self.member_authority_live(
            envelope,
            &member_call,
            MemberReprove {
                operation,
                scope: want_scope,
                job_id,
                op_credential_id: None,
            },
            verb,
        )?;
        // Lazily provision the assistant runtime + private conversation
        // once the member is proven live — a member's first verb brings up
        // their real agent and binds its private thread.
        let binding = self.ensure_assistant_runtime(&binding)?;
        let member_call = MemberCaller {
            conversation_id: binding.conversation_id,
            ..member_call
        };
        Ok(member_call)
    }

    /// `member_assist_state` — the member's own assistant surface state.
    pub(super) fn rpc_member_assist_state(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_unknown_fields(params, &["member_envelope"], "member assist state")?;
        let member = self.member_caller(
            params,
            peer_pid,
            "member assist state",
            "assist_state",
            crate::assistant_actor::AssistScope::Read,
            None,
        )?;
        let binding = self
            .store
            .assistant_binding(&member.org, &member.sub)?
            .ok_or_else(|| Error::rejected("assistant binding vanished"))?;
        Ok(json!({
            "assistant": binding.to_json(),
            "conversation_id": member.conversation_id,
        }))
    }

    /// `member_assist_history` — a PAGED read of the member's own
    /// assistant conversation: the stored binding's thread id (never a
    /// request field) → the thread's entries after `after` (or the newest
    /// `tail` page), bounded by `limit`. A member sees ONLY their bound
    /// conversation's entries — no other principal's chat, never a forged
    /// or caller-named thread.
    pub(super) fn rpc_member_assist_history(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_unknown_fields(
            params,
            &["member_envelope", "after", "before", "tail", "limit"],
            "member assist history",
        )?;
        let member = self.member_caller(
            params,
            peer_pid,
            "member assist history",
            "assist_history",
            crate::assistant_actor::AssistScope::Read,
            None,
        )?;
        let conv = member.conversation_id.clone().ok_or_else(|| {
            Error::rejected("the member's assistant has no bound conversation yet")
        })?;
        // The bound thread must still be this assistant alias's and
        // unarchived — a foreign or archived thread is never read.
        let thread = self
            .store
            .thread_by_id(&conv)?
            .filter(|t| t.alias == member.alias && !t.archived)
            .ok_or_else(|| Error::rejected("the bound conversation is not this assistant's"))?;
        let limit = optional_i64(params, "limit")
            .unwrap_or(100)
            .clamp(1, store::THREAD_PAGE_MAX);
        let tail = params.get("tail").and_then(Value::as_bool).unwrap_or(false);
        let before = optional_i64(params, "before");
        if tail || before.is_some() {
            let (entries, more) = self
                .store
                .thread_entries_before_of(&thread.id, before, limit)?;
            let cursor = entries.last().map(|e| e.seq).unwrap_or(0);
            return Ok(json!({
                "conversation": thread.to_json(),
                "entries": entries.iter().map(store::ThreadEntry::to_json).collect::<Vec<_>>(),
                "cursor": cursor,
                "more_before": more,
            }));
        }
        let after = optional_i64(params, "after").unwrap_or(0);
        if after < 0 {
            return Err(Error::rejected("history cursor must be nonnegative"));
        }
        let entries = self.store.thread_entries_of(&thread.id, after, limit)?;
        let cursor = entries.last().map(|e| e.seq).unwrap_or(after);
        Ok(json!({
            "conversation": thread.to_json(),
            "entries": entries.iter().map(store::ThreadEntry::to_json).collect::<Vec<_>>(),
            "cursor": cursor,
        }))
    }

    /// `member_assist_send` — the member sends to their own assistant's
    /// bound conversation only; the target alias + conversation are the
    /// stored binding's, never a request field.
    pub(super) fn rpc_member_assist_send(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        reject_unknown_fields(params, &["member_envelope", "text"], "member assist send")?;
        let member = self.member_caller(
            params,
            peer_pid,
            "member assist send",
            "assist_send",
            crate::assistant_actor::AssistScope::Write,
            None,
        )?;
        let text = required_str(params, "text")?;
        if text.trim().is_empty() {
            return Err(Error::rejected("member assist send text is empty"));
        }
        // The send lands in the assistant alias's own thread, attributed to
        // the MEMBER (`Sender::Member(sub)`) — never `Sender::OperatorChat`,
        // which would launder the member's text into operator authority and
        // could carry an app binding. The conversation is the bound thread;
        // a member supplies no `app`/`conversation` selector.
        let send = json!({
            "alias": member.alias,
            "text": text,
            "source": format!("member-{}", member.sub),
        });
        let mut receipt =
            self.send_as(&send, &|_| Ok(store::Sender::Member(member.sub.clone())))?;
        // The send is accepted and queued. Now bring up the assistant's
        // own managed endpoint so a real confined turn drains it — the
        // ONLY member verb that may launch, for the ONE bound alias the
        // verified envelope + live authority just proved. A launch
        // refusal never fails the send: the row stays queued and the
        // receipt names the truthful cause.
        if let Err(e) = self.member_assist_launch(&member) {
            receipt["queued"] = json!(true);
            receipt["attention"] = json!(format!("assistant endpoint did not start: {e}"));
        }
        Ok(receipt)
    }

    /// Lazily start the verified member's own assistant endpoint after a
    /// live `assist_send` admission. The alias is the stored binding's —
    /// never a caller field; the agent row is the confined `pi`/`managed`
    /// record the daemon's own `ensure_assistant_runtime` wrote, so no
    /// request picks the provider, cwd, or launch params.
    ///
    /// Launch posture (daemon-derived only):
    /// - the model is resolved from the operator's `[pi]` policy for the
    ///   `worker` role — `[pi].models.default.worker`, strictly gated by
    ///   `require_allowed` (no platform-fallback bypass), stamped onto
    ///   the row only when the agent still carries none. A confined
    ///   `devin/*` pick refuses at `refuse_confined_devin` — the same
    ///   precondition `agent_register` enforces.
    /// - `try_resume` carries the real guards: `lc.owned` serializes
    ///   concurrent sends to at most one launch; an unreconciled
    ///   `unknown` fence refuses — a fenced assistant stays fenced, the
    ///   queued row waiting for the operator reconcile.
    ///
    /// Returns `Err` (truthful, bounded — never a fabricated reply) when
    /// the endpoint could not start; the caller reports it on the
    /// receipt while the message remains queued. `Ok(())` means an
    /// actor now owns the alias (it may still fence on `open` — that
    /// lands `attention` like every managed launch).
    fn member_assist_launch(self: &Arc<Self>, member: &MemberCaller) -> Result<()> {
        let alias = &member.alias;
        // Fail closed on the fence first — an unreconciled unknown means
        // the endpoint may hold an uncertain turn; never relaunch it.
        if self.store.has_unknown(alias)? {
            return Err(Error::rejected(format!(
                "assistant '{alias}' is fenced by an unreconciled unknown outcome — \
                 the operator reconciles before it runs again"
            )));
        }
        // At most one launch across concurrent sends: an owned alias is
        // success, not a refusal — its live actor drains the queue.
        if self.lifecycle.lock().unwrap().owned(alias) {
            return Ok(());
        }
        // Stamp the launch model if the agent still carries none — the
        // registration writes no model so this first write happens here,
        // before `open` (which refuses an empty pi model). Daemon-
        // resolved, operator-allowlisted, `set_params` re-derives the
        // model_selection provenance honestly. A caller can never name
        // it: `member_assist_send` carries only `member_envelope`+`text`.
        let agent = self.store.agent(alias)?;
        let has_model = agent
            .params
            .as_ref()
            .and_then(|p| p.get("model"))
            .and_then(Value::as_str)
            .is_some_and(|m| !m.trim().is_empty());
        if !has_model {
            let pm_dir = self.pm_dir().ok().filter(|d| d.is_dir());
            let pi_policy = match &pm_dir {
                Some(dir) => crate::pi_policy::read(dir)?,
                None => None,
            };
            let model = crate::pi_policy::resolve_model(pi_policy.as_ref(), "worker", None)
                .map_err(|e| {
                    Error::rejected(format!(
                        "assistant endpoint has no permitted pi worker model configured \
                         ([pi].models): {e}"
                    ))
                })?;
            // A confined assistant may not run a Devin model — pi-devin
            // needs the operator's credential Landlock cannot grant.
            crate::adapter::pi::refuse_confined_devin(alias, true, &model)?;
            self.store
                .set_params(alias, &json!({"model": model}))
                .map_err(|e| {
                    Error::rejected(format!("assistant launch model could not be recorded: {e}"))
                })?;
        }
        // Start the real managed actor — inbox check, ownership, fence
        // and `start_actor_locked` all inside `try_resume`. A refusal
        // surfaces truthfully; `Ok(false)` means the fence check inside
        // `start_actor_locked` raced and fenced — still truthful.
        match self.try_resume(alias) {
            Ok(_) => Ok(()),
            Err(e) => {
                // A concurrent send may have won the launch between the
                // owned check and the call — owned now is a live actor,
                // not a failure.
                if self.lifecycle.lock().unwrap().owned(alias) {
                    Ok(())
                } else {
                    Err(e)
                }
            }
        }
    }

    /// `member_job_request` — bounded shared-job request, idempotent on
    /// `(org, sub, request_key)`. The canonical job id is derived from
    /// `request_key` — NOT the wall clock — so two DISTINCT request_keys
    /// (even in the same second) mint two distinct jobs, while a replayed
    /// or concurrent equivalent on the SAME request_key returns the one
    /// canonical job (acceptance: idempotency is same-request-key, not
    /// same-second).
    ///
    /// A real `plans::Job` is created: `pm_alias` is the member's own
    /// bound assistant alias (the assistant REQUESTS and follows the
    /// job — it is NOT an authorized approver/dispatcher), `state`
    /// `open`, and a draft task carries the member's title/acceptance.
    /// The request persists its title/body/acceptance + authorization
    /// provenance so the deferred commit can re-prove the member's
    /// authority before any dispatch lands (SEC-002 / RISK-001). Nothing
    /// auto-dispatches: `plan_gate_task` and the operator/PM dispatch
    /// rules still gate the kickoff — a member cannot self-approve.
    pub(super) fn rpc_member_job_request(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_unknown_fields(
            params,
            &[
                "member_envelope",
                "title",
                "request_key",
                "body",
                "acceptance",
            ],
            "member job request",
        )?;
        let member = self.member_caller(
            params,
            peer_pid,
            "member job request",
            "job_request",
            crate::assistant_actor::AssistScope::Jobs,
            None,
        )?;
        let title = required_str(params, "title")?;
        let request_key = required_str(params, "request_key")?;
        if title.trim().is_empty() || request_key.trim().is_empty() {
            return Err(Error::rejected(
                "member job request needs a nonempty title and request_key",
            ));
        }
        if request_key.len() > 200
            || !request_key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
        {
            return Err(Error::rejected(
                "request_key must be <= 200 chars of the id grammar",
            ));
        }
        let body = optional_str(params, "body").unwrap_or_default();
        let acceptance = optional_str(params, "acceptance").unwrap_or_default();
        // Idempotent: the stored (org,sub,request_key) → job mapping wins.
        if let Some(job) = self
            .store
            .assist_request_job(&member.org, &member.sub, request_key)?
        {
            let existing = self.store.job(&job).ok().map(|j| j.to_json());
            return Ok(json!({
                "job": job,
                "canonical": true,
                "requested_by": member.sub,
                "status": "existing",
                "job_record": existing,
            }));
        }
        // The canonical job id is unique per (org,sub,request_key):
        // derived from the request_key digest so distinct request_keys in
        // the same second are distinct jobs. `assist_request_bind`'s
        // INSERT-OR-IGNORE + read-back makes the concurrent equivalent
        // resolve to the committed winner — one lane per request_key.
        let key_digest =
            crate::operator_auth::digest(&format!("{}:{}:{}", member.org, member.sub, request_key));
        let job_id = format!("assist-{}", &key_digest[..key_digest.len().min(24)]);
        // Persist the request's authorization provenance BEFORE creating
        // the job — the deferred commit re-proves exactly this.
        let spec_path = self.assist_job_spec(&job_id, title, body, acceptance)?;
        let spec_sha = sha256_file(std::path::Path::new(&spec_path))
            .ok_or_else(|| Error::internal("assist job spec could not be hashed"))?;
        // Create the real job bound to the assistant alias as its PM
        // (the assistant tracks, never auto-dispatches). No issue_id —
        // the member cannot mint a tracker lane; an approved promotion
        // binds one through the normal PM/operator path.
        let (_dup, _job) = self.store.create_job(
            &job_id,
            Some(title),
            &spec_path,
            &spec_sha,
            &member.alias,
            None,
            None,
            None,
            2,
            None,
            Some(title),
            None,
            None,
            None,
            None, // no assignee: a draft task awaits the operator's dispatch
            if acceptance.is_empty() {
                None
            } else {
                Some(acceptance)
            },
        )?;
        let bound = self.store.assist_request_bind(
            &member.org,
            &member.sub,
            request_key,
            &job_id,
            title,
            body,
            acceptance,
            &member.credential_id,
        )?;
        // The requester auto-subscribes to their own canonical job —
        // the subscriber ACL covers the requester too.
        self.store
            .job_subscribe(&bound, &member.org, &member.sub, &member.assistant_id)?;
        let record = self.store.job(&bound).ok().map(|j| j.to_json());
        Ok(json!({
            "job": bound,
            "canonical": bound == job_id,
            "requested_by": member.sub,
            "assistant": member.assistant_id,
            "status": "awaiting_approval",
            "job_record": record,
            "note": "the request is recorded for operator/PM approval — \
                     it dispatches no work itself",
        }))
    }

    /// Write the member's job request to a bounded spec file the job
    /// records — daemon-owned storage, never a caller path. The spec
    /// carries the title/body/acceptance verbatim so the kickoff reads
    /// exactly what the member asked for.
    fn assist_job_spec(
        &self,
        job_id: &str,
        title: &str,
        body: &str,
        acceptance: &str,
    ) -> Result<String> {
        let dir = self.state_dir.join("assist_jobs");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{job_id}.md"));
        let text = format!("# {title}\n\n{body}\n\n## Acceptance\n\n{acceptance}\n");
        // Same redaction a stored chat body gets — a member's free text is
        // scanned before it becomes a job spec.
        let redacted = match crate::secret::redact_text(&text) {
            Ok(r) => r,
            Err(_) => "[withheld: secret scan unavailable]".to_string(),
        };
        let mut bounded = redacted;
        if bounded.len() > 48_000 {
            let mut cut = 48_000;
            while cut > 0 && !bounded.is_char_boundary(cut) {
                cut -= 1;
            }
            bounded.truncate(cut);
        }
        std::fs::write(&path, bounded)?;
        Ok(path.to_string_lossy().to_string())
    }

    /// `member_job_list` — the jobs this member owns or is subscribed to.
    pub(super) fn rpc_member_job_list(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_unknown_fields(params, &["member_envelope"], "member job list")?;
        let member = self.member_caller(
            params,
            peer_pid,
            "member job list",
            "job_list",
            crate::assistant_actor::AssistScope::Jobs,
            None,
        )?;
        let jobs = self.store.assist_member_jobs(&member.org, &member.sub)?;
        Ok(json!({ "jobs": jobs }))
    }

    /// `member_job_evidence` — revision-bound evidence for a bound job,
    /// only for the owner or a subscriber. Evidence states
    /// `reported`/`inspected`/`independently_verified`/`unverified`/
    /// `unavailable` stay distinct and bind to the actual submitted
    /// revision, never self-report.
    pub(super) fn rpc_member_job_evidence(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_unknown_fields(params, &["member_envelope", "job"], "member job evidence")?;
        let member = self.member_caller(
            params,
            peer_pid,
            "member job evidence",
            "job_evidence",
            crate::assistant_actor::AssistScope::Jobs,
            params.get("job").and_then(Value::as_str),
        )?;
        let job = required_str(params, "job")?;
        let owns = self
            .store
            .assist_request_owns(&member.org, &member.sub, job)?;
        let sub = self
            .store
            .job_is_subscriber(job, &member.org, &member.sub)?;
        if !(owns || sub) {
            return Err(Error::rejected(format!(
                "member job evidence refused: '{job}' is not a job this member \
                 owns or subscribes to"
            )));
        }
        // REAL evidence, revision-bound: the job's tasks carry the actual
        // submitted head_sha + revision; the verdict row binds the exact
        // reviewed sha. `reported` = a submitted revision with no verdict;
        // `inspected` = a verdict recorded against the submitted sha
        // (revise/blocked counted as inspected, pass as independently
        // verified — an author-independent reviewer judged the exact
        // revision); `unverified` = a draft/dispatched task with no
        // submitted revision yet; `unavailable` = no task or the recorded
        // artifact is gone. Nothing here fabricates a pass.
        let job_row = match self.store.job(job) {
            Ok(j) => j,
            Err(_) => {
                return Ok(json!({
                    "job": job,
                    "evidence_state": "unavailable",
                    "revision": Value::Null,
                    "note": "the job record is not resolvable",
                }))
            }
        };
        let tasks = self.store.tasks_for_job(job)?;
        let mut task_evidence = Vec::new();
        let mut any_state = "reported";
        for task in &tasks {
            let verdicts = self.store.verdicts_for_task(&task.id)?;
            let current = store::current_verdict(task.revision, &verdicts);
            let (state, sha, reviewer) = match (&task.head_sha, current) {
                (Some(sha), Some(v)) if v.sha == *sha => (
                    match v.verdict.as_str() {
                        "pass" => "independently_verified",
                        _ => "inspected",
                    },
                    Some(sha.clone()),
                    Some(v.reviewer.clone()),
                ),
                (Some(sha), _) => ("reported", Some(sha.clone()), None),
                (None, Some(v)) => ("inspected", Some(v.sha.clone()), Some(v.reviewer.clone())),
                // A task with no submitted head and no verdict is still
                // `reported` — the member's request itself is the
                // evidence record. `unverified` is reserved for a
                // stale/missing artifact on submitted work, never a
                // fresh lane's normal state.
                (None, None) => ("reported", None, None),
            };
            if matches!(state, "independently_verified" | "inspected") {
                any_state = state;
            }
            task_evidence.push(json!({
                "task": task.id,
                "state": task.state,
                "revision": task.revision,
                "head_sha": task.head_sha,
                "evidence_state": state,
                "verdict": current.map(|v| v.to_json()),
                "reviewer": reviewer,
                "sha": sha,
            }));
        }
        let revision = tasks.iter().map(|t| t.revision).max().unwrap_or(0);
        // The current pinned authority revision — the value a
        // `member_job_reconcile` must name. It is re-derived from the
        // job's CURRENT `updated` watermark on every read, so a member
        // always sees the revision covering the queued effect as it
        // stands NOW (a moved job yields a different rev; a stale-viewed
        // reconcile then refuses).
        let authority_rev = self
            .store
            .assist_job_authority_rev(job)?
            .unwrap_or_default();
        Ok(json!({
            "job": job,
            "job_state": job_row.state,
            "job_title": job_row.title,
            "evidence_state": any_state,
            "authority_rev": authority_rev,
            "revision": revision,
            "tasks": task_evidence,
            "note": "evidence binds to the actual submitted revision and verdict; \
                     a member read never fabricates verification",
        }))
    }

    /// `member_job_events` — the subscriber-visible event feed for a bound
    /// job: the job's own event stream (plan/task/dispatch/verdict events),
    /// never private chat text. Same ACL as `member_job_evidence`.
    pub(super) fn rpc_member_job_events(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_unknown_fields(
            params,
            &["member_envelope", "job", "after", "tail"],
            "member job events",
        )?;
        let member = self.member_caller(
            params,
            peer_pid,
            "member job events",
            "job_events",
            crate::assistant_actor::AssistScope::Jobs,
            params.get("job").and_then(Value::as_str),
        )?;
        let job = required_str(params, "job")?;
        let sub = self
            .store
            .job_is_subscriber(job, &member.org, &member.sub)?;
        let owns = self
            .store
            .assist_request_owns(&member.org, &member.sub, job)?;
        if !(sub || owns) {
            return Err(Error::rejected(format!(
                "member job events refused: '{job}' is not a job this member \
                 owns or subscribes to"
            )));
        }
        // The job's own event stream — work metadata only (kind, revision,
        // task, verdict), never a member's private conversation.
        let after = optional_i64(params, "after").unwrap_or(0);
        if after < 0 {
            return Err(Error::rejected("event cursor must be nonnegative"));
        }
        if params.get("tail").and_then(Value::as_bool).unwrap_or(false) {
            let mut events = self.store.job_events_tail(job, 51)?;
            let has_older = events.len() > 50;
            if has_older {
                events.remove(0);
            }
            return Ok(json!({
                "job": job,
                "events": events.iter().map(store::Event::to_json).collect::<Vec<_>>(),
                "cursor": events.last().map(|e| e.seq).unwrap_or(0),
                "has_older": has_older,
            }));
        }
        let events = self.store.job_events(job, after, 200)?;
        Ok(json!({
            "job": job,
            "events": events.iter().map(store::Event::to_json).collect::<Vec<_>>(),
            "cursor": events.last().map(|e| e.seq).unwrap_or(after),
        }))
    }

    /// `member_job_share` — the OWNING member enables or disables
    /// sharing of their own assist-bound job inside their org (the
    /// entitlement a second member's `member_job_subscribe` requires).
    /// Only the member who created the job through `member_job_request`
    /// (`assist_request_owns`) may share it — a second member can never
    /// mark someone else's job shared, and a share on a job bound to a
    /// different org is impossible because `owns` is org+sub-bound.
    /// `enabled` (default true) writes or clears the
    /// `assist_job_shares(job, org)` row.
    pub(super) fn rpc_member_job_share(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_unknown_fields(
            params,
            &["member_envelope", "job", "enabled"],
            "member job share",
        )?;
        let job = required_str(params, "job")?;
        let enabled = params
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let member = self.member_caller(
            params,
            peer_pid,
            "member job share",
            "job_share",
            crate::assistant_actor::AssistScope::Jobs,
            Some(job),
        )?;
        if !self
            .store
            .assist_request_owns(&member.org, &member.sub, job)?
        {
            return Err(Error::rejected(format!(
                "member job share refused: '{job}' is not a job this member owns"
            )));
        }
        self.store
            .assist_job_share(job, &member.org, &member.sub, enabled)?;
        Ok(json!({
            "job": job,
            "shared": enabled,
            "shared_by": member.sub,
            "note": if enabled {
                "the job is now shareable inside the member's organization — \
                 a second member may subscribe"
            } else {
                "sharing is off — no NEW subscription is admitted (existing \
                 subscribers are not retro-removed)"
            },
        }))
    }

    /// `member_job_reconcile {job, expected_authority_rev}` — the
    /// OWNING member's explicit job-specific authorized reconciliation:
    /// re-pin a deferred assist job's recorded provenance
    /// (`assist_requests.credential_id`) to the member's CURRENT live
    /// credential (the signed envelope's `credential_id` claim —
    /// `member_caller` has already re-proven that credential live for
    /// THIS call). Only the member who owns the job
    /// (`assist_request_owns`) may reconcile it — the recorded
    /// provenance NEVER migrates silently at mint or dispatch.
    ///
    /// `expected_authority_rev` is a REQUIRED pin — the exact revision
    /// the member observed for this canonical request (`member_job_
    /// evidence` reports it as `authority_rev`, bound to the job's
    /// current `updated` watermark so it covers the queued effect's
    /// operation/task revision). The compare-and-update runs in ONE
    /// store transaction: a stale rev, a wrong-owner rev, or a
    /// mismatched job refuses with NO change — an old viewed decision
    /// can never reauthorize a new queued effect, and a replayed or
    /// out-of-order reconcile never silently re-pins.
    ///
    /// After a reconcile the deferred dispatch carries the NEW
    /// `credential_id` — `member_authority_live`'s `job_dispatch`
    /// re-prove then re-checks THAT credential live. A queued effect
    /// whose own credential is dead stays denied until THIS verb runs.
    pub(super) fn rpc_member_job_reconcile(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_unknown_fields(
            params,
            &["member_envelope", "job", "expected_authority_rev"],
            "member job reconcile",
        )?;
        let job = required_str(params, "job")?;
        let member = self.member_caller(
            params,
            peer_pid,
            "member job reconcile",
            "job_reconcile",
            crate::assistant_actor::AssistScope::Jobs,
            Some(job),
        )?;
        if !self
            .store
            .assist_request_owns(&member.org, &member.sub, job)?
        {
            return Err(Error::rejected(format!(
                "member job reconcile refused: '{job}' is not a job this member owns"
            )));
        }
        // The REQUIRED revision pin is checked only after ownership — a
        // non-owner learns nothing about the rev grammar, and an owner
        // without the current rev is refused before any write.
        let expected_rev = required_str(params, "expected_authority_rev").map_err(|_| {
            Error::rejected(
                "member job reconcile refused: expected_authority_rev is required — \
                 pin the job's current committed revision",
            )
        })?;
        if expected_rev.len() > 64
            || !expected_rev
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        {
            return Err(Error::rejected(
                "expected_authority_rev must be a short revision token",
            ));
        }
        // Re-pin the recorded provenance to the member's CURRENT live
        // credential (the envelope this call carried) — bound to the
        // exact revision the member named, compared inside the same
        // transaction that writes: a stale or wrong rev refuses with no
        // change. The deferred dispatch re-prove names this exact
        // credential — a queued effect that was denied under the old
        // credential now commits under the member's proven CURRENT
        // authority, explicitly authorized by the owner.
        let new_rev = self.store.assist_request_reconcile(
            &member.org,
            &member.sub,
            job,
            &member.credential_id,
            expected_rev,
        )?;
        Ok(json!({
            "job": job,
            "reconciled": member.sub,
            "credential_id": member.credential_id,
            "authority_rev": new_rev,
            "note": "the job's deferred effect now carries the member's \
                     current provenance — the deferred dispatch re-prove \
                     names this credential",
        }))
    }

    /// `member_job_subscribe` — a SECOND member subscribes to a shared
    /// assist-bound job in their own org (AOS-216 #3 / CAD-1380 #3):
    /// "an authorized second member can subscribe to the same shared job
    /// without obtaining the first member's private conversation or
    /// creating duplicate execution". The subscription is the read-ACL
    /// grant — after it, `member_job_evidence`/`member_job_events` admit
    /// the member for THIS job (and the supervisor's `assist_stall`
    /// fan-out reaches their bound assistant alias).
    ///
    /// Entitlement (SEC-002): org membership + `assist.jobs` scope is
    /// NOT enough — the job's OWNING member must have explicitly shared
    /// it via `member_job_share` (an `assist_job_shares` row in the
    /// member's org). A member can never mint the entitlement by naming
    /// a job id; the `job` is a resource selector, never an authority.
    ///
    /// Refusals: a job that is NOT assist-bound (no `assist_requests`
    /// row — an ordinary/foreign lane can never become a member-visible
    /// share), an assist job bound to a DIFFERENT org (cross-org leak),
    /// or a same-org assist job the owner has NOT shared.
    pub(super) fn rpc_member_job_subscribe(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_unknown_fields(params, &["member_envelope", "job"], "member job subscribe")?;
        let job = required_str(params, "job")?;
        let member = self.member_caller(
            params,
            peer_pid,
            "member job subscribe",
            "job_subscribe",
            crate::assistant_actor::AssistScope::Jobs,
            Some(job),
        )?;
        // The job must be an assist-bound job in THIS member's org. A
        // missing `assist_requests` row (not assist-bound) refuses; a row
        // naming a different org refuses — a member can only subscribe
        // inside their own organization's shared work.
        let assist = self.store.assist_request_for_job(job)?.ok_or_else(|| {
            Error::rejected(format!(
                "member job subscribe refused: '{job}' is not an assist-bound job"
            ))
        })?;
        if assist.org != member.org {
            return Err(Error::rejected(format!(
                "member job subscribe refused: '{job}' is bound to another organization"
            )));
        }
        // The job record must exist — a subscribe never names a
        // non-existent or stale id.
        let record = self.store.job(job).ok().map(|j| j.to_json());
        if record.is_none() {
            return Err(Error::rejected(format!(
                "member job subscribe refused: '{job}' has no job record"
            )));
        }
        // The entitlement: the job's owner must have explicitly shared
        // it inside THIS member's org. Org membership + `assist.jobs`
        // never substitutes for the owner's authenticated grant.
        if !self.store.assist_job_is_shared(job, &member.org)? {
            return Err(Error::rejected(format!(
                "member job subscribe refused: '{job}' is not shared by its owner"
            )));
        }
        self.store
            .job_subscribe(job, &member.org, &member.sub, &member.assistant_id)?;
        Ok(json!({
            "job": job,
            "subscribed": member.sub,
            "assistant": member.assistant_id,
            "job_record": record,
            "note": "the member now follows this shared job — evidence and \
                     events bind to it; no private conversation is shared",
        }))
    }
}
