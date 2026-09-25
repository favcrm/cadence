//! CAD-506 / ADR 0006 §5.2, §5.4: the effect gate and the
//! pending-effect lifecycle.
//!
//! `platform_call` is the only way a managed call reaches a platform.
//! The gate classifies the call from the adapter's reviewed tool table
//! and the manifest version the *platform* reports — never from a call
//! argument (C1–C3). `read` and `draft` execute at once through the
//! proxy; `send` never executes here — it stages a durable
//! pending-effect row and answers `staged` + `effect_id`.
//!
//! A staged send is released only by the press (`agent respond` on the
//! brokered handle): accept is the operator's alone in v1, decline is
//! anyone authorised (the operator or the requester's PM), an agent
//! never releases. The decision lands durably BEFORE execution; the
//! platform write carries the `effect_id` as its idempotency key and
//! the pinned `source_hash` is re-verified inside Execute — so a
//! source edit, a revoke or a restart can never turn an approved send
//! into a second or a stale fire.
//!
//! The durable row is the authority; `pending`/`answered` never hold
//! an effect. `request_wait`/`request_close` on an effect handle only
//! end the caller's own wait — the row outlives it. A restart
//! reconciles `decided`/`executing` rows to `reconcile` (never
//! re-fired); `waiting` rows simply wait on.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use super::{optional_str, required_str, AgentCaller, Shared};
use crate::contract_fixture::{classify_call, Effect};
use crate::error::{Error, Result};
use crate::platform::{self, PlatformAdapter};
use crate::proto::identifier;
use crate::store::{self, EffectKey, EffectRow};

/// The byte bound on one staged `input` (§5.4: the proxy applies a
/// bound; the exact number is the implementation's).
const INPUT_CAP: usize = 64 * 1024;

/// The record's `input_summary` cap (schema: one bounded line).
const SUMMARY_CAP: usize = 1024;

/// The record's `preview` cap (schema: bounded rendered artifact).
const PREVIEW_CAP: usize = 16 * 1024;

/// The bound on durable free text a caller authors — `decision.reason`
/// and `close_reason` (N2).
const REASON_CAP: usize = 1024;

/// `s.truncate(cap)` walks the cap back to a char boundary first —
/// `String::truncate` panics mid-char and every capped string here is
/// built from caller-controlled UTF-8 (`tool`, `input`, `reason`).
fn cap_str(s: &mut String, cap: usize) {
    let mut end = cap.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

impl Shared {
    /// `platform_call {platform, tool, input?, account?, task?,
    /// request?}` — an agent's platform call. The caller is the
    /// connection-derived agent ([`Self::request_caller`]): no field
    /// names it, `input.effect` never classifies, and `input.source`
    /// only names a reviewed artifact — never the hash. `account`
    /// defaults like `platform_check`'s: the caller's project default
    /// for the platform.
    pub(super) fn rpc_platform_call(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let agent = self.request_caller(params, peer_pid, "platform call")?;
        // The caller is the connection — `agent`/`alias` are request
        // fields only where they name a target; a platform call has no
        // such field, so any present is refused, even one naming the
        // caller itself (§5.4: the record's agent is never a field).
        for field in ["agent", "alias"] {
            if params.get(field).is_some() {
                return Err(Error::rejected(format!(
                    "platform call: caller identity is connection-bound; request \
                     field '{field}' is not accepted"
                )));
            }
        }
        self.store.agent(&agent)?;
        let platform = identifier(required_str(params, "platform")?, "Platform")?;
        let tool = required_str(params, "tool")?;
        let input = match params.get("input") {
            None | Some(Value::Null) => json!({}),
            Some(v) if v.is_object() => v.clone(),
            Some(_) => return Err(Error::rejected("'input' must be a JSON object")),
        };
        let input_bytes = serde_json::to_string(&input)?.len();
        if input_bytes > INPUT_CAP {
            return Err(Error::rejected(format!(
                "platform call input is {input_bytes} bytes — over the {INPUT_CAP} bound"
            )));
        }
        let account = match optional_str(params, "account") {
            Some(a) => identifier(a, "Account")?,
            None => self.default_account(&agent, &platform, params)?,
        };
        let task = optional_str(params, "task")
            .map(|t| identifier(t, "Task"))
            .transpose()?;
        let adapter = self.platform_adapter(&platform)?;
        // The §5.3 grant check gates even staging — a caller holding no
        // grant on the account learns that before a row exists.
        self.check_grant(&agent, &platform, &account, &adapter, tool)?;
        // The §5.4 cancel scan runs on every touchpoint: a source edit
        // since the row staged cancels it before this call proceeds.
        self.scan_effect_sources(Some(&agent));

        let reported = adapter.reported_manifest_version();
        let table = adapter.table();
        match classify_call(table, reported.as_deref(), tool) {
            Effect::Read | Effect::Draft => {
                let draft = table.effect_of(tool) == Effect::Draft;
                self.execute_immediate(&agent, &platform, &account, tool, &input, draft, &adapter)
            }
            Effect::Send => self.stage_send(
                &agent, &platform, &account, tool, &input, task, params, &adapter,
            ),
        }
    }

    /// The caller's project default account for `platform` (the same
    /// resolution `platform_check` documents): `params.project`, else
    /// the caller agent's cwd's project.
    fn default_account(&self, agent: &str, platform: &str, params: &Value) -> Result<String> {
        let project = match optional_str(params, "project") {
            Some(p) => identifier(p, "Project")?,
            None => {
                let agent_row = self.store.agent(agent)?;
                crate::issue::project::key_for_cwd(
                    &self.pm_dir()?,
                    std::path::Path::new(&agent_row.cwd),
                )
                .ok_or_else(|| {
                    Error::rejected(format!(
                        "'{agent}'s cwd resolves to no project — pass 'account' \
                         or 'project'"
                    ))
                })?
            }
        };
        self.store
            .platform_default(&project, platform)?
            .map(|d| d.account)
            .ok_or_else(|| {
                Error::rejected(format!(
                    "project '{project}' names no default account for '{platform}' — \
                     pass 'account'"
                ))
            })
    }

    /// The registered adapter for `platform` — a platform with none
    /// refuses every call (the gate fails closed, §5.1).
    fn platform_adapter(&self, platform: &str) -> Result<Arc<dyn PlatformAdapter>> {
        self.platforms.get(platform).cloned().ok_or_else(|| {
            Error::rejected(format!(
                "no adapter is registered for platform '{platform}' — the gate \
                 cannot classify the call"
            ))
        })
    }

    /// §5.3 for one call: every declared scope must sit in the agent's
    /// grant. An undeclared tool has no declared scopes — the agent
    /// must still hold *a* grant on the account.
    fn check_grant(
        &self,
        agent: &str,
        platform_name: &str,
        account: &str,
        adapter: &Arc<dyn PlatformAdapter>,
        tool: &str,
    ) -> Result<()> {
        let scopes = adapter
            .table()
            .declared(tool)
            .map(|d| d.scopes.clone())
            .unwrap_or_default();
        self.check_scopes(agent, platform_name, account, &scopes)
    }

    /// One grant covering `scopes` — the same check at stage and at
    /// execute (the frozen row scopes, not the live table).
    fn check_scopes(
        &self,
        agent: &str,
        platform_name: &str,
        account: &str,
        scopes: &[String],
    ) -> Result<()> {
        if scopes.is_empty() {
            return match self.store.platform_grant(agent, platform_name, account)? {
                Some(_) => Ok(()),
                None => Err(Error::rejected(format!(
                    "'{agent}' holds no grant on {platform_name}/{account} — the \
                     operator grants with `cadence platform grant`"
                ))),
            };
        }
        for scope in scopes {
            platform::require_grant(&self.store, agent, platform_name, account, scope)?;
        }
        Ok(())
    }

    /// `read`/`draft`: execute through the proxy at once. A draft also
    /// lands in the durable draft log — the information-only "ran
    /// without you" row (§5.2, Q3).
    #[allow(clippy::too_many_arguments)]
    fn execute_immediate(
        &self,
        agent: &str,
        platform_name: &str,
        account: &str,
        tool: &str,
        input: &Value,
        draft: bool,
        adapter: &Arc<dyn PlatformAdapter>,
    ) -> Result<Value> {
        let bytes =
            platform::load_credential(&self.store, &self.platform_custody, platform_name, account)?;
        let key = format!("call-{}", uuid::Uuid::new_v4().simple());
        let result = adapter
            .execute(&bytes, tool, input, &key, None)
            .map_err(Error::internal)?;
        platform::refuse_leak("platform result", &result.to_string(), &bytes)?;
        let label = adapter.table().declared(tool).and_then(|d| d.label.clone());
        let summary = input_summary(tool, input);
        if draft {
            self.store.draft_record(
                &store::DraftRow {
                    id: 0,
                    agent: agent.to_string(),
                    platform: platform_name.to_string(),
                    account: account.to_string(),
                    tool: tool.to_string(),
                    label,
                    input_summary: summary.clone(),
                    artifact: result
                        .get("platform_ref")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    ran_at: epoch_now(),
                },
                &result,
            )?;
        }
        // The agent-lane event is read by any caller (`agent_events` is
        // an unscoped read): it carries the call's identity fields
        // only — input-derived text stays on the scoped record (I1).
        let _ = self.store.event_public(
            agent,
            "platform_called",
            json!({"platform": platform_name, "account": account,
                   "tool": tool, "effect": if draft { "draft" } else { "read" }}),
        );
        self.wake();
        Ok(json!({
            "result": "executed",
            "effect": if draft { "draft" } else { "read" },
            "platform_result": result,
        }))
    }

    /// `send`: stage the durable row and answer at once — the call
    /// never touches the platform. `input.source` pins the reviewed
    /// artifact's hash; a caller naming a source the platform does not
    /// hold is refused rather than staged unpinned.
    #[allow(clippy::too_many_arguments)]
    fn stage_send(
        &self,
        agent: &str,
        platform_name: &str,
        account: &str,
        tool: &str,
        input: &Value,
        task: Option<String>,
        params: &Value,
        adapter: &Arc<dyn PlatformAdapter>,
    ) -> Result<Value> {
        let source_name = input
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_string);
        let source_hash = match &source_name {
            Some(name) => Some(adapter.source_hash(name).ok_or_else(|| {
                Error::rejected(format!(
                    "input.source names '{name}' — the platform holds no such \
                     reviewed artifact; the send cannot be pinned"
                ))
            })?),
            None => None,
        };
        let bytes =
            platform::load_credential(&self.store, &self.platform_custody, platform_name, account)?;
        let decl = adapter.table().declared(tool);
        let summary = input_summary(tool, input);
        let mut preview = adapter.preview(account, tool, input);
        if preview.len() > PREVIEW_CAP {
            cap_str(&mut preview, PREVIEW_CAP);
        }
        // Secret-guarded before anything lands: input, summary and
        // preview must never carry the enrolled bytes.
        platform::refuse_leak("staged input", &input.to_string(), &bytes)?;
        platform::refuse_leak("input summary", &summary, &bytes)?;
        platform::refuse_leak("preview", &preview, &bytes)?;
        // `request` is the caller-named brokered handle — a retry with
        // the same handle dedupes to the staged row (never a second
        // effect). The durable id derives from it so both names stay
        // stable across restart.
        let request = match optional_str(params, "request") {
            Some(h) => identifier(h, "Request handle")?,
            None => format!("req-{}", uuid::Uuid::new_v4().simple()),
        };
        // One handle = one request across both registries — the mirror
        // of `request_open`'s effect guard: a live brokered entry or a
        // parked answer already owns this handle (locked in respond's
        // order: pending → answered).
        {
            let pending = self.pending.lock().unwrap();
            let answered = self.answered.lock().unwrap();
            if pending.contains_key(&request) || answered.contains_key(&request) {
                return Err(Error::rejected(format!(
                    "platform call refused: handle '{request}' already names a \
                     brokered request — a handle belongs to one request (CAD-506)"
                )));
            }
        }
        let effect_id = format!("eff-{}", request.strip_prefix("req-").unwrap_or(&request));
        let row = EffectRow {
            effect_id: effect_id.clone(),
            request: request.clone(),
            agent: agent.to_string(),
            platform: platform_name.to_string(),
            account: account.to_string(),
            tool: tool.to_string(),
            label: decl.and_then(|d| d.label.clone()),
            input: input.clone(),
            input_summary: summary,
            preview,
            source_name,
            source_hash,
            scopes: decl.map(|d| d.scopes.clone()).unwrap_or_default(),
            task,
            state: "waiting".to_string(),
            close_reason: None,
            decision: None,
            outcome: None,
            needs_you: false,
            staged_at: epoch_now(),
            updated_at: epoch_now(),
        };
        let (row, existing) = self.store.effect_stage(&row)?;
        // The agent-lane `request_opened` IS `effect_requested` (§5.5):
        // the durable event the press follows. Emitted only for a fresh
        // stage — a deduped retry adds no second event.
        if !existing {
            // `agent_events` is an unscoped `Rule::Read` — a peer (or
            // any unproven caller) reads this lane. The event carries
            // the handle the press follows plus routing fields only;
            // the staged input's text stays on the scoped record (I1).
            let _ = self.store.event_public(
                agent,
                "request_opened",
                json!({"request": row.request, "kind": "effect", "tool": tool,
                       "platform": platform_name, "account": account}),
            );
        }
        self.wake();
        Ok(json!({
            "result": if existing { "existing" } else { "staged" },
            "existing": existing,
            "effect_id": row.effect_id,
            "request": row.request,
            "state": row.state,
            "record": row.to_record(),
        }))
    }

    /// `platform_effects {agent?, limit?}` — the pending-effect and
    /// draft read model. An agent caller sees its own rows; the
    /// operator sees all, or one agent's. The read first runs the
    /// cancel scan (§5.4) so a board that polls sees edits as closed
    /// rows, never stale approvals.
    pub(super) fn rpc_platform_effects(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let caller = self.agent_caller(peer_pid, "platform effects")?;
        let filter = match &caller {
            AgentCaller::Operator => match optional_str(params, "agent") {
                Some(a) => Some(identifier(a, "Agent")?),
                None => None,
            },
            AgentCaller::Agent(alias) => {
                if let Some(named) = optional_str(params, "agent") {
                    if named != alias.as_str() {
                        return Err(Error::rejected(format!(
                            "platform effects refused: '{alias}' sees its own pending \
                             effects — '{named}' is another agent's"
                        )));
                    }
                }
                Some(alias.clone())
            }
        };
        self.scan_effect_sources(filter.as_deref());
        let limit = super::optional_u64(params, "limit").unwrap_or(50) as usize;
        let rows = self.store.platform_effects(filter.as_deref())?;
        let effects: Vec<Value> = rows.iter().map(EffectRow::to_record).collect();
        // Needs-you rows ride their own array — the record itself is
        // the §5.4 schema shape only (needs_you is row bookkeeping).
        let needs_you: Vec<Value> = rows
            .iter()
            .filter(|r| r.needs_you)
            .map(|r| {
                json!({"effect_id": r.effect_id, "request": r.request,
                       "agent": r.agent, "tool": r.tool, "state": r.state,
                       "platform": r.platform, "account": r.account})
            })
            .collect();
        let drafts: Vec<Value> = self
            .store
            .platform_drafts(filter.as_deref(), limit)?
            .iter()
            .map(store::DraftRow::to_json)
            .collect();
        Ok(json!({"effects": effects, "needs_you": needs_you, "drafts": drafts}))
    }

    /// `platform_effect_close {request|effect_id, reason?}` — cancel a
    /// `waiting` row, or resolve a `reconcile` row after a human has
    /// inspected the platform. The operator closes any; an agent closes
    /// only its own `waiting` row (it staged it — it may unstage it).
    /// Nothing past the durable decision is closeable: `decided` is in
    /// flight and terminals stay terminal.
    pub(super) fn rpc_platform_effect_close(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let caller = self.agent_caller(peer_pid, "platform effect close")?;
        let key = match optional_str(params, "request") {
            Some(r) => EffectKey::Request(identifier(r, "Request handle")?),
            None => EffectKey::Id(identifier(required_str(params, "effect_id")?, "Effect id")?),
        };
        let reason = optional_str(params, "reason")
            .unwrap_or("cancelled")
            .to_string();
        // Which row the caller may reach: the operator any closeable
        // row; an agent only its own `waiting` ones — a `reconcile` row
        // is exactly the ambiguity only the operator resolves.
        let rows = self.store.platform_effects(None)?;
        let row = rows.iter().find(|r| match &key {
            EffectKey::Request(req) => &r.request == req,
            EffectKey::Id(id) => &r.effect_id == id,
        });
        let Some(row) = row else {
            return Err(Error::rejected("no pending effect by that handle"));
        };
        match &caller {
            AgentCaller::Operator => {}
            AgentCaller::Agent(alias) if alias == &row.agent && row.state == "waiting" => {}
            AgentCaller::Agent(alias) => {
                return Err(Error::rejected(format!(
                    "platform effect close refused: agent '{alias}' closes only its own \
                     waiting effects — {} is {}/'{}''s (caller rule, CAD-506)",
                    row.request, row.state, row.agent
                )))
            }
        }
        // A flagged terminal row (verified:false outcome) is not
        // closeable — but the operator's close on it acknowledges the
        // Needs-you: the item clears, the record stands.
        if row.needs_you && matches!(row.state.as_str(), "done" | "failed") {
            if !matches!(caller, AgentCaller::Operator) {
                return Err(Error::rejected(
                    "platform effect close refused: a Needs-you flag is the \
                     operator's to acknowledge (caller rule, CAD-506)",
                ));
            }
            let row = self
                .store
                .effect_ack(&row.effect_id)?
                .unwrap_or_else(|| row.clone());
            return Ok(json!({"state": "acknowledged", "record": row.to_record()}));
        }
        // Caller-authored free text lands durably — screen it against
        // the row's enrolled credential and bound it like every other
        // stored string (N2). A custody miss just skips the screen.
        let mut reason = reason;
        if let Ok(bytes) = platform::load_credential(
            &self.store,
            &self.platform_custody,
            &row.platform,
            &row.account,
        ) {
            platform::refuse_leak("close reason", &reason, &bytes)?;
        }
        if reason.len() > REASON_CAP {
            cap_str(&mut reason, REASON_CAP);
        }
        let Some(row) = self.store.effect_close(key, &reason)? else {
            return Err(Error::rejected(format!(
                "effect {} is no longer closeable (already decided or terminal)",
                row.effect_id
            )));
        };
        self.emit_request_closed(&row.agent, &row.request, &reason);
        self.wake();
        Ok(json!({"state": "closed", "record": row.to_record()}))
    }

    /// The press — `agent respond` on a staged effect's handle. The
    /// caller was already authorised by [`Self::authorize_respond`]
    /// (the operator or the requester's PM — the requester and peers
    /// never reach here). `accept` additionally demands the proven
    /// operator: release is operator-only in v1 (C6). A refusal leaves
    /// the row untouched; a second press finds the row no longer
    /// `waiting` and is refused.
    pub(super) fn respond_effect(
        self: &Arc<Self>,
        row: &EffectRow,
        peer_pid: u32,
        decision: Option<&str>,
        answers: &Option<Value>,
        reason: Option<String>,
        named_alias: &str,
    ) -> Result<Value> {
        if named_alias != row.agent {
            return Err(Self::foreign_request(
                "agent respond",
                named_alias,
                &row.agent,
            ));
        }
        if answers.is_some() {
            return Err(Error::rejected("Respond with decision accept or decline"));
        }
        let accept = match decision {
            Some("accept") => true,
            Some("decline") => false,
            _ => return Err(Error::rejected("Respond with decision accept or decline")),
        };
        let caller = self.agent_caller(peer_pid, "agent respond")?;
        if accept && !matches!(caller, AgentCaller::Operator) {
            let who = match &caller {
                AgentCaller::Agent(a) => format!("agent '{a}'"),
                AgentCaller::Operator => unreachable!(),
            };
            return Err(Error::rejected(format!(
                "agent respond refused: {who} cannot release an effect — the \
                 press accept is operator-only in v1 (ADR 0006 §5.4 step 4, C6)"
            )));
        }
        // The presser's free-text reason lands on the durable row —
        // screen it against the row's enrolled credential and bound it
        // like every other stored string (N2). A custody miss just
        // skips the screen.
        let reason = reason
            .map(|mut r| {
                if let Ok(bytes) = platform::load_credential(
                    &self.store,
                    &self.platform_custody,
                    &row.platform,
                    &row.account,
                ) {
                    platform::refuse_leak("decision reason", &r, &bytes)?;
                }
                if r.len() > REASON_CAP {
                    cap_str(&mut r, REASON_CAP);
                }
                Ok::<String, Error>(r)
            })
            .transpose()?;
        // `by` is {member, role, rule}: who pressed, the role they
        // hold, the rule that authorised the press — {member, role}
        // comes from the proven caller and the agent row, never a
        // request field.
        let rule = if accept {
            "operator-only"
        } else {
            "authorised-decline"
        };
        let by = match &caller {
            AgentCaller::Operator => store::presser_json("operator", "operator", rule),
            AgentCaller::Agent(alias) => {
                // The record's role is the §5.4 vocabulary — operator /
                // pm / agent — not the row's launch role: a pm-row agent
                // presses as "pm", any other agent as "agent".
                let role = self
                    .store
                    .agent(alias)
                    .map(|a| if a.role == "pm" { "pm" } else { "agent" })
                    .unwrap_or("agent");
                store::presser_json(alias, role, rule)
            }
        };
        let mut decided =
            json!({"by": by, "at": crate::issue::time::iso(crate::issue::time::now_epoch())});
        if let Some(reason) = &reason {
            decided["reason"] = json!(reason);
        }
        // The atomic decide is the concurrent-press claim: whichever
        // transaction lands first owns the row; every other press sees
        // it no longer waiting and is refused (§5.4 step 4).
        let Some(row) = self.store.effect_decide(&row.request, accept, &decided)? else {
            return Err(Error::rejected(
                "this pending effect is no longer waiting — it was already \
                 decided, declined or closed",
            ));
        };
        if !accept {
            self.deliver_effect_outcome(&row, "declined");
            self.emit_request_closed(&row.agent, &row.request, "declined");
            self.wake();
            return Ok(json!({"state": "answered", "effect": row.to_record()}));
        }
        // The decision is durable; the test gate models the daemon
        // dying inside step 5's window — the row stays `decided` for
        // restart reconciliation (C7). Production never gates here.
        if self
            .effect_execute_gate
            .as_ref()
            .is_some_and(|gate| !gate(&row))
        {
            return Ok(json!({"state": "answered", "effect": row.to_record()}));
        }
        match self.execute_effect(&row) {
            Ok(row) => Ok(json!({"state": "answered", "effect": row.to_record()})),
            Err(err) => Err(err),
        }
    }

    /// Execute the accepted send — §5.4 steps 3–6. The durable
    /// `decided` row exists already; this runs: source re-verify →
    /// grant re-check → custody load → `executing` → the platform call
    /// under the `effect_id` idempotency key and expected hash →
    /// adapter read-back → the recorded outcome → the delivered
    /// message. A source change, a lost grant or a credential that no
    /// longer loads closes the row instead of firing; a platform error
    /// lands `failed`, never `closed`.
    fn execute_effect(&self, row: &EffectRow) -> Result<EffectRow> {
        let adapter = match self.platforms.get(&row.platform).cloned() {
            Some(a) => a,
            None => {
                return self.fail_effect(
                    row,
                    &format!("no adapter is registered for platform '{}'", row.platform),
                );
            }
        };
        // §5.4 step 3's race clause: the pinned hash is re-verified
        // inside Execute — an edit the waiting scan missed still
        // cancels the send here, before any traffic.
        if let Some(source) = &row.source_name {
            let now_hash = adapter.source_hash(source);
            if now_hash.as_deref() != row.source_hash.as_deref() {
                let row = self
                    .store
                    .effect_close_decided(&row.effect_id, "source_changed")?
                    .unwrap_or(row.clone());
                self.emit_request_closed(&row.agent, &row.request, "source_changed");
                self.deliver_effect_outcome(&row, "closed (source_changed)");
                self.wake();
                return Ok(row);
            }
        }
        if let Err(err) = self.check_scopes(&row.agent, &row.platform, &row.account, &row.scopes) {
            let row = self
                .store
                .effect_close_decided(&row.effect_id, "grant_revoked")?
                .unwrap_or(row.clone());
            self.emit_request_closed(&row.agent, &row.request, "grant_revoked");
            self.deliver_effect_outcome(&row, "closed (grant_revoked)");
            self.wake();
            return Err(Error::rejected(format!(
                "effect {} closed grant_revoked: {err}",
                row.effect_id
            )));
        }
        // Custody load runs before the `executing` marker: an
        // `executing` row means the platform call may already have
        // fired (reconcile territory), while a credential that will
        // not load means the call provably never ran — the still-
        // `decided` row closes `credential_revoked` and the requester
        // hears about it like every other execute-time close (N3).
        let bytes = match platform::load_credential(
            &self.store,
            &self.platform_custody,
            &row.platform,
            &row.account,
        ) {
            Ok(b) => b,
            Err(_) => {
                let row = self
                    .store
                    .effect_close_decided(&row.effect_id, "credential_revoked")?
                    .unwrap_or(row.clone());
                self.emit_request_closed(&row.agent, &row.request, "credential_revoked");
                self.deliver_effect_outcome(&row, "closed (credential_revoked)");
                self.wake();
                return Ok(row);
            }
        };
        self.store.effect_executing(&row.effect_id)?;
        let outcome = adapter.execute(
            &bytes,
            &row.tool,
            &row.input,
            &row.effect_id,
            row.source_hash.as_deref(),
        );
        // Read-back runs whatever the call did: a platform error is
        // `failed`, a landed-but-divergent write is `done` with
        // verified:false (Needs-you), "unknown" is a legitimate steady
        // state (C10).
        let verified = adapter.read_back(&row.tool, &row.input);
        let (ok, outcome) = match outcome {
            Ok(result) => {
                platform::refuse_leak("platform outcome", &result.to_string(), &bytes)?;
                (true, json!({"result": result, "verified": verified}))
            }
            Err(error) => {
                platform::refuse_leak("platform error", &error, &bytes)?;
                (false, json!({"error": error, "verified": verified}))
            }
        };
        let summary = if ok {
            format!("{} executed", row.tool)
        } else {
            format!(
                "{} failed: {}",
                row.tool,
                outcome["error"].as_str().unwrap_or("?")
            )
        };
        let row = self
            .store
            .effect_outcome(&row.effect_id, ok, &outcome, &summary)?;
        self.deliver_effect_outcome(&row, if ok { "done" } else { "failed" });
        self.emit_request_closed(
            &row.agent,
            &row.request,
            if ok { "answered" } else { "failed" },
        );
        self.wake();
        Ok(row)
    }

    /// An outcome the platform never saw — daemon-side failure. The row
    /// lands `failed` with `verified:"unknown"`: the run could not even
    /// be attempted, which is a Needs-you-grade ambiguity.
    fn fail_effect(&self, row: &EffectRow, error: &str) -> Result<EffectRow> {
        self.store.effect_executing(&row.effect_id)?;
        let outcome = json!({"error": error, "verified": "unknown"});
        let row = self
            .store
            .effect_outcome(&row.effect_id, false, &outcome, error)?;
        self.deliver_effect_outcome(&row, "failed");
        self.wake();
        Ok(row)
    }

    /// §5.4 step 6's delivery: the outcome reaches the task's lane —
    /// the requester's PM, else the requester itself — as a durable
    /// daemon message, deduped by the `effect_id` whether or not a
    /// caller is still waiting.
    fn deliver_effect_outcome(&self, row: &EffectRow, state: &str) {
        let to = self
            .upstream_of(&row.agent)
            .unwrap_or_else(|| row.agent.clone());
        let verified = row
            .outcome
            .as_ref()
            .map(|o| {
                if o["verified"] == Value::Bool(false) {
                    "; read-back MISMATCH — Needs-you"
                } else {
                    ""
                }
            })
            .unwrap_or_default();
        let mut body = format!(
            "platform effect {} {}: {} on {}/{} — {}{}\npreview: {}",
            row.effect_id,
            state,
            row.tool,
            row.platform,
            row.account,
            row.input_summary,
            verified,
            row.preview,
        );
        // An adapter may surface a board link in its result (CAD-546's
        // local outbox does): relay it bounded, and only an http(s)
        // URL is ever carried into a lane message.
        if let Some(url) = row
            .outcome
            .as_ref()
            .and_then(|o| o.get("result"))
            .and_then(|r| r.get("board_url"))
            .and_then(Value::as_str)
            .filter(|u| u.len() <= 512 && (u.starts_with("https://") || u.starts_with("http://")))
        {
            body.push_str(&format!("\nboard: {url}"));
        }
        let id = crate::proto::daemon_message_id("effect", &row.effect_id);
        // A `task` the call named that no longer exists must not drop
        // the outcome — retry untagged (the dedupe id is the same; the
        // first send wins either way).
        let delivered = self
            .store
            .enqueue_daemon_task(&to, &body, &id, "effect", row.task.as_deref())
            .or_else(|_| {
                self.store
                    .enqueue_daemon_task(&to, &body, &id, "effect", None)
            })
            .is_ok();
        if delivered {
            self.notify_agent(&to);
        }
    }

    /// The agent-lane close event mirroring `request_closed` — a
    /// `kind:"effect"` handle ending without a press answer.
    fn emit_request_closed(&self, agent: &str, request: &str, reason: &str) {
        let _ = self.store.event_public(
            agent,
            "request_closed",
            json!({"request": request, "kind": "effect", "reason": reason}),
        );
    }

    /// §5.4's waiting-row cancel scan: re-hash the pinned source of
    /// each waiting row (all of them, or one agent's) against what the
    /// platform reports now; a drift closes the row `source_changed`.
    /// Best-effort — a platform that cannot report leaves its rows
    /// alone (a missing hash proves nothing; the Execute re-check still
    /// catches the race before traffic).
    pub(super) fn scan_effect_sources(&self, agent: Option<&str>) {
        let Ok(rows) = self.store.platform_effects(agent) else {
            return;
        };
        for row in rows {
            if row.state != "waiting" {
                continue;
            }
            let Some(source) = &row.source_name else {
                continue;
            };
            let changed = self
                .platforms
                .get(&row.platform)
                .is_some_and(|a| a.source_hash(source).as_deref() != row.source_hash.as_deref());
            if changed {
                if let Ok(Some(closed)) = self
                    .store
                    .effect_close(EffectKey::Id(row.effect_id.clone()), "source_changed")
                {
                    self.emit_request_closed(&closed.agent, &closed.request, "source_changed");
                }
            }
        }
    }

    /// `request_wait`'s durable fallback: the handle names a staged
    /// effect — the row, not the caller's deadline, owns the lifecycle.
    /// Answers `None` when the handle is no effect's (the caller then
    /// sees plain `closed`).
    pub(super) fn effect_wait(&self, handle: &str, caller: &str) -> Result<Option<Value>> {
        let Some(row) = self.store.effect_by_request(handle)? else {
            return Ok(None);
        };
        if row.agent != caller {
            return Err(Self::foreign_request("request_wait", caller, &row.agent));
        }
        Ok(Some(effect_wait_state(&row)))
    }

    /// `request_close`'s durable fallback: the caller's own deadline —
    /// it ends the wait, never the row (§5.4 step 7). No event, no
    /// state write: nothing about the effect changed.
    pub(super) fn effect_caller_close(&self, handle: &str, caller: &str) -> Result<Option<Value>> {
        let Some(row) = self.store.effect_by_request(handle)? else {
            return Ok(None);
        };
        if row.agent != caller {
            return Err(Self::foreign_request("request_close", caller, &row.agent));
        }
        Ok(Some(match row.state.as_str() {
            "done" | "failed" => {
                json!({"state": "answered", "answer": row.outcome.clone().unwrap_or(Value::Null)})
            }
            "waiting" | "decided" | "executing" => json!({"state": "closed"}),
            _ => json!({"state": "closed",
                        "reason": row.close_reason.clone().unwrap_or_else(|| row.state.clone())}),
        }))
    }
}

/// What `request_wait` reports for a live effect row.
fn effect_wait_state(row: &EffectRow) -> Value {
    match row.state.as_str() {
        "done" | "failed" => json!({"state": "answered",
            "answer": row.outcome.clone().unwrap_or(Value::Null)}),
        "waiting" | "decided" | "executing" => json!({"state": "waiting"}),
        "reconcile" => json!({"state": "closed", "reason": "reconciling — operator review"}),
        _ => json!({"state": "closed",
            "reason": row.close_reason.clone().unwrap_or_else(|| row.state.clone())}),
    }
}

/// `input_summary` — one bounded line for list surfaces, derived from
/// the staged input (never caller-supplied): `tool k=v … (source)` when
/// every value is a string, `tool {json}` otherwise; `input.source`
/// renders as the trailing `(name)`.
fn input_summary(tool: &str, input: &Value) -> String {
    let source = input.get("source").and_then(Value::as_str);
    let rest: Vec<(&String, &Value)> = input
        .as_object()
        .map(|m| m.iter().filter(|(k, _)| k.as_str() != "source").collect())
        .unwrap_or_default();
    let body = if rest.iter().all(|(_, v)| v.is_string()) {
        rest.iter()
            .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or_default()))
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        let obj: serde_json::Map<String, Value> = rest
            .iter()
            .map(|(k, v)| ((*k).clone(), (*v).clone()))
            .collect();
        serde_json::to_string(&Value::Object(obj)).unwrap_or_default()
    };
    let mut s = match (body.is_empty(), source) {
        (true, None) => tool.to_string(),
        (true, Some(src)) => format!("{tool} ({src})"),
        (false, None) => format!("{tool} {body}"),
        (false, Some(src)) => format!("{tool} {body} ({src})"),
    };
    if s.len() > SUMMARY_CAP {
        cap_str(&mut s, SUMMARY_CAP);
    }
    s
}

fn epoch_now() -> f64 {
    crate::issue::time::now_epoch() as f64
}

/// The adapter map `ServeOptions::platforms` becomes.
pub(crate) type PlatformMap = HashMap<String, Arc<dyn PlatformAdapter>>;

/// The test-only crash seam between the durable `decided` write and
/// execution (`ServeOptions::effect_execute_gate`).
pub(crate) type EffectExecuteGate = Arc<dyn Fn(&EffectRow) -> bool + Send + Sync>;
