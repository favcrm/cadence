//! Strict operator management of versioned campaign email content (CAD-782)
//! plus the host-verified assistant proposal handoff (CAD-813).
//!
//! Every operator method first proves the operator connection, then resolves
//! the installation through the workspace catalog snapshot — an
//! unknown or diverted installation ID never reaches a file — and
//! proves the live context on core, reads included: an unknown,
//! archived or foreign context refuses before any file opens, and
//! only then opens the installation's record file. The payload
//! grammar is exact: identity-shaped (`by`, `actor`),
//! receipt-shaped (`assistant_receipt`, `turn_id`, `nonce`),
//! discovery-link (`project`, `project_link`) and routing
//! (`workspace`) fields are unsupported and refused, as are the
//! URL-scoped IDs themselves when they appear in a body. Subjects,
//! blocks, tokens and button URLs are validated by the store's
//! allowlisted grammar; no HTML is ever stored or rendered except
//! the host's own fixed template. Proposals submitted on the operator
//! path are recorded `actor='operator'`: the connection proves the
//! operator submitted the draft, never that an assistant produced
//! it. `assistant` attribution arrives only through
//! `rpc_app_content_assistant_propose` (CAD-813), which derives the
//! agent from the connection alone, binds its live assigned chat turn
//! (`message` + `token`) and re-proves the turn's server-verified App
//! binding against the store, then redeems the operator-minted,
//! host-stamped proposal request (`request_id`): campaign and source
//! revision come from the stamp alone, never agent text — the browser
//! may request or display a proposal but can never mint its
//! provenance. The operator mints requests through
//! `app_content_proposal_request` on this same operator path. Sender material renders
//! only through typed preview-only bindings; final-send preparation
//! always refuses until CAD-785/786 supply host-verified evidence.
//! There is no agent-origin edit/approve/send path: proposals, Apply,
//! Discard, approval and send preparation all require the operator
//! connection, so an agent caller or detached child is refused without
//! mutation. The board peer lives in `src/ui/app_content.rs`; it
//! follows the CAD-768 strict-peer contract (URL IDs are authority,
//! exact transport grammar, POST-only writes) and exposes no
//! assistant-mint route.
use super::*;
use crate::issue::app_catalog::workspace;
use crate::store::app_content::Draft;
use crate::store::app_records::RecordStore;

fn content_draft(params: &Value) -> Result<Draft> {
    let subject = required_str(params, "subject")?;
    let preheader = match params.get("preheader") {
        None => "",
        Some(Value::String(text)) => text.as_str(),
        Some(_) => {
            return Err(Error::rejected("email preheader must be a string"));
        }
    };
    let blocks = params
        .get("blocks")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::rejected("email blocks must be an array"))?;
    Draft::parse(subject, preheader, blocks)
        .map_err(|_| Error::rejected("email content exceeds its supported shape or bounds"))
}

fn content_expected(params: &Value) -> Result<Option<i64>> {
    match params.get("expected_revision") {
        None => Ok(None),
        Some(Value::Number(number)) => Ok(Some(
            number
                .as_u64()
                .and_then(|value| i64::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or_else(|| Error::rejected("expected revision must be a positive integer"))?,
        )),
        Some(_) => Err(Error::rejected(
            "expected revision must be a positive integer",
        )),
    }
}

fn content_revision(params: &Value) -> Result<i64> {
    params
        .get("expected_revision")
        .and_then(Value::as_u64)
        .and_then(|value| i64::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| Error::rejected("expected revision must be a positive integer"))
}

fn content_binding(params: &Value) -> Result<Option<&str>> {
    match params.get("binding_id") {
        None => Ok(None),
        Some(Value::String(id)) => Ok(Some(id.as_str())),
        Some(_) => Err(Error::rejected("sender binding ID must be a string")),
    }
}

fn content_render_scope(params: &Value) -> Result<(Option<i64>, Option<String>)> {
    let revision = match params.get("revision") {
        None => None,
        Some(Value::Number(number)) => Some(
            number
                .as_u64()
                .and_then(|value| i64::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or_else(|| Error::rejected("email content revision must be positive"))?,
        ),
        Some(_) => {
            return Err(Error::rejected("email content revision must be positive"));
        }
    };
    let sample = match params.get("sample_first_name") {
        None => None,
        Some(Value::String(name)) => Some(name.clone()),
        Some(_) => {
            return Err(Error::rejected("email sample name must be a string"));
        }
    };
    Ok((revision, sample))
}

fn content_request_id(params: &Value) -> Result<&str> {
    required_str(params, "request_id")
}

impl Shared {
    pub(super) fn rpc_app_content(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("app content management", params, peer_pid)?;
        // Receipt-shaped fields are refused everywhere: assistant
        // attribution requires a CAD-784 chat receipt, and no such
        // receipt exists yet, so any present receipt is forged.
        if let Some(fields) = params.as_object() {
            if fields
                .keys()
                .any(|key| matches!(key.as_str(), "assistant_receipt" | "turn_id" | "nonce"))
            {
                return Err(Error::rejected(
                    "app content payload has unsupported fields",
                ));
            }
        }
        let allowed: &[&str] = match method {
            "app_sender_binding_save" => &[
                "install_id",
                "context_id",
                "binding_id",
                "sender_name",
                "sender_address",
                "unsubscribe_base",
                "connection_id",
                "expected_revision",
            ],
            "app_sender_binding_show" => &["install_id", "context_id", "binding_id"],
            "app_sender_binding_list" => &["install_id", "context_id"],
            "app_content_save" => &[
                "install_id",
                "context_id",
                "campaign_id",
                "subject",
                "preheader",
                "blocks",
                "expected_revision",
            ],
            "app_content_show" => &["install_id", "context_id", "campaign_id"],
            "app_content_list" => &["install_id", "context_id"],
            "app_content_render" => &[
                "install_id",
                "context_id",
                "campaign_id",
                "revision",
                "sample_first_name",
                "binding_id",
            ],
            "app_content_propose" => &[
                "install_id",
                "context_id",
                "campaign_id",
                "proposal_id",
                "subject",
                "preheader",
                "blocks",
            ],
            "app_content_proposal_request" => &[
                "install_id",
                "context_id",
                "campaign_id",
                "message",
                "request_id",
            ],
            "app_content_proposal_show" => &["install_id", "context_id", "proposal_id"],
            "app_content_proposal_list" => &["install_id", "context_id", "campaign_id"],
            "app_content_proposal_apply" => &[
                "install_id",
                "context_id",
                "proposal_id",
                "expected_revision",
            ],
            "app_content_proposal_discard" => &["install_id", "context_id", "proposal_id"],
            "app_content_approve" => &[
                "install_id",
                "context_id",
                "campaign_id",
                "expected_revision",
            ],
            "app_content_test_prepare" => &[
                "install_id",
                "context_id",
                "campaign_id",
                "to_email",
                "binding_id",
            ],
            "app_content_send_prepare" => &[
                "install_id",
                "context_id",
                "campaign_id",
                "binding_id",
                "audience_freeze_id",
            ],
            _ => return Err(Error::rejected("unknown app content method")),
        };
        let fields = params
            .as_object()
            .ok_or_else(|| Error::rejected("app content payload must be an object"))?;
        if fields.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err(Error::rejected(
                "app content payload has unsupported fields",
            ));
        }
        let install = required_str(params, "install_id")?;
        crate::proto::identifier(install, "installation ID")?;
        let context = required_str(params, "context_id")?;
        let pm = self.pm_at(&self.pm_dir()?)?;
        let write = !matches!(
            method,
            "app_content_show"
                | "app_content_list"
                | "app_content_render"
                | "app_content_proposal_show"
                | "app_content_proposal_list"
                | "app_sender_binding_show"
                | "app_sender_binding_list"
        );
        workspace::with_runtime_snapshot(&pm, install, |_, _| {
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            // Reads prove the live context too: an unknown, archived
            // or foreign context refuses before any file opens.
            self.store.app_context_proof(install, context)?;
            Ok(())
        })?;
        let records = RecordStore::open(&self.state_dir, install)?;
        let result = match method {
            "app_content_save" => records.app_content_save(
                context,
                required_str(params, "campaign_id")?,
                content_expected(params)?,
                &content_draft(params)?,
            ),
            "app_content_show" => {
                records.app_content_show(context, required_str(params, "campaign_id")?)
            }
            "app_content_list" => records.app_content_list(context),
            "app_sender_binding_save" => {
                let connection = match params.get("connection_id") {
                    None => None,
                    Some(Value::String(id)) => Some(id.as_str()),
                    Some(_) => {
                        return Err(Error::rejected("sender connection ID must be a string"));
                    }
                };
                let draft = crate::store::app_content::BindingDraft {
                    sender_name: required_str(params, "sender_name")?,
                    sender_address: required_str(params, "sender_address")?,
                    unsubscribe_base: required_str(params, "unsubscribe_base")?,
                    connection_id: connection,
                };
                records.app_sender_binding_save(
                    context,
                    required_str(params, "binding_id")?,
                    content_expected(params)?,
                    &draft,
                )
            }
            "app_sender_binding_show" => {
                records.app_sender_binding_show(context, required_str(params, "binding_id")?)
            }
            "app_sender_binding_list" => records.app_sender_binding_list(context),
            "app_content_render" => {
                let (revision, sample) = content_render_scope(params)?;
                records.app_content_render(
                    context,
                    required_str(params, "campaign_id")?,
                    revision,
                    sample.as_deref(),
                    content_binding(params)?,
                )
            }
            "app_content_propose" => records.app_content_propose(
                context,
                required_str(params, "campaign_id")?,
                required_str(params, "proposal_id")?,
                &content_draft(params)?,
            ),
            // CAD-813: the operator mints a one-time proposal
            // request against a chat message. The message must exist
            // and carry the server-verified App binding for this
            // installation and context — the stamp is host scope,
            // never a browser value. Campaign and source revision
            // are stamped here and re-proved at redemption.
            "app_content_proposal_request" => {
                let message_id = required_str(params, "message")?;
                if message_id.is_empty()
                    || message_id.len() > 128
                    || message_id.chars().any(char::is_control)
                {
                    return Err(Error::rejected("proposal message identity is malformed"));
                }
                self.store
                    .message(message_id)?
                    .ok_or_else(|| Error::rejected("proposal request message is unknown"))?;
                let hint = self.store.message_app(message_id)?.ok_or_else(|| {
                    Error::rejected("proposal request message carries no verified App scope")
                })?;
                if hint.get("install_id").and_then(Value::as_str) != Some(install)
                    || hint.get("context_id").and_then(Value::as_str) != Some(context)
                {
                    return Err(Error::rejected(
                        "proposal request scope does not match its verified chat message",
                    ));
                }
                records.app_content_proposal_request(
                    context,
                    required_str(params, "campaign_id")?,
                    message_id,
                    content_request_id(params)?,
                )
            }
            "app_content_proposal_show" => {
                records.app_content_proposal_show(context, required_str(params, "proposal_id")?)
            }
            "app_content_proposal_list" => {
                let campaign = match params.get("campaign_id") {
                    None => None,
                    Some(Value::String(id)) => Some(id.as_str()),
                    Some(_) => {
                        return Err(Error::rejected("campaign ID must be a string"));
                    }
                };
                records.app_content_proposal_list(context, campaign)
            }
            "app_content_proposal_apply" => records.app_content_proposal_apply(
                context,
                required_str(params, "proposal_id")?,
                content_expected(params)?,
            ),
            "app_content_proposal_discard" => {
                records.app_content_proposal_discard(context, required_str(params, "proposal_id")?)
            }
            "app_content_approve" => records.app_content_approve(
                context,
                required_str(params, "campaign_id")?,
                content_revision(params)?,
            ),
            "app_content_test_prepare" => records.app_content_test_prepare(
                context,
                required_str(params, "campaign_id")?,
                required_str(params, "to_email")?,
                content_binding(params)?,
            ),
            "app_content_send_prepare" => {
                let freeze = match params.get("audience_freeze_id") {
                    None => None,
                    Some(Value::String(id)) => {
                        crate::proto::identifier(id, "freeze ID")?;
                        Some(id.as_str())
                    }
                    Some(_) => {
                        return Err(Error::rejected("audience freeze ID must be a string"));
                    }
                };
                records.app_content_send_prepare(
                    context,
                    required_str(params, "campaign_id")?,
                    required_str(params, "binding_id")?,
                    freeze,
                )
            }
            _ => Err(Error::rejected("unknown app content method")),
        }?;
        if write {
            // Best-effort audit on core; digests only, never content.
            let digest = result
                .get("binding")
                .and_then(|binding| binding.get("binding_digest"))
                .or_else(|| {
                    result
                        .get("content")
                        .and_then(|doc| doc.get("content_digest"))
                })
                .or_else(|| {
                    result
                        .get("proposal")
                        .and_then(|proposal| proposal.get("content_digest"))
                })
                .or_else(|| {
                    result
                        .get("test_send")
                        .and_then(|send| send.get("content_digest"))
                })
                .or_else(|| {
                    result
                        .get("send")
                        .and_then(|send| send.get("content_digest"))
                })
                .and_then(Value::as_str)
                .unwrap_or("");
            self.store
                .note_app_content(install, context, method, digest);
            self.wake();
        }
        Ok(result)
    }

    /// CAD-813: the host-verifiable assistant proposal handoff — an
    /// assigned agent turn's only write. The agent is derived from the
    /// connection alone (never a request field); the operator is
    /// refused here (it proposes through `app_content_propose`) and a
    /// detached child is unproven. The turn is bound by the daemon's
    /// own rows: `message` must address the caller, hold its one live
    /// turn (`running`, `turn_id == token`, current under the
    /// endpoint's own token scheme), and carry the turn's
    /// server-verified App binding (`message_app` re-proved) naming
    /// exactly this installation and context. The installation
    /// resolves through the workspace catalog snapshot and the live
    /// context is proved on core before any file opens, exactly as on
    /// the operator path. `source_revision`, when named, must equal
    /// the current draft revision — a stale draft refuses. The stored
    /// proposal is inert (`pending`, `assistant` /
    /// `assistant-receipt`) until the operator explicitly applies or
    /// discards it; this verb can never edit, approve, test-send or
    /// send. The browser can never reach this verb with an agent
    /// caller: the board relays from its own operator process, which
    /// this gate refuses. Campaign and source revision are compared
    /// against the operator-minted, host-stamped proposal request
    /// the call redeems — never against agent text — and the request
    /// is spent atomically across all proposal ids.
    pub(super) fn rpc_app_content_assistant_propose(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let fields = params
            .as_object()
            .ok_or_else(|| Error::rejected("app content payload must be an object"))?;
        const ALLOWED: &[&str] = &[
            "install_id",
            "context_id",
            "campaign_id",
            "proposal_id",
            "subject",
            "preheader",
            "blocks",
            "message",
            "token",
            "request_id",
        ];
        if fields.keys().any(|key| !ALLOWED.contains(&key.as_str())) {
            return Err(Error::rejected(
                "app content payload has unsupported fields",
            ));
        }
        // The caller is the connection's alone: identity-shaped,
        // receipt-shaped and routing fields are not transport fields
        // here at all — the allowlist above already refused them —
        // and the agent is never named, only derived.
        let caller = match self.connection_caller(peer_pid)? {
            caller_rule::Who::Agent(alias) => alias,
            caller_rule::Who::Operator => {
                return Err(Error::rejected(
                    "assistant proposal is an agent turn's verb — the operator proposes through app_content_propose",
                ));
            }
            caller_rule::Who::Unproven(why) => {
                return Err(Error::rejected(format!(
                    "assistant proposal refused: this connection derives no agent identity and is \
                     not provably the operator: {why} (caller rule, CAD-384)"
                )));
            }
        };
        // Endpoint-session bind: pane ancestry alone also derives a
        // `setsid`-detached child that kept the pane on its ancestry,
        // so the caller must additionally live in the endpoint's own
        // session — exactly the `app_run_capability_call` rule. Under
        // a test-seam agent assertion the seam stands in for ancestry
        // (production builds carry no seam) and only the turn proofs
        // below decide.
        let seam_agent = matches!(
            crate::test_seam::asserted(),
            Some(crate::test_seam::Asserted::Agent(ref alias)) if alias == &caller
        );
        if !seam_agent {
            let caller_session = crate::peer::proc_session(peer_pid)
                .map_err(|_| Error::rejected("assistant proposal caller session is unreadable"))?;
            let inside = match self.slot_identity(peer_pid)? {
                Some(SlotWho::Strict(proof)) => {
                    let root = *proof.segment.last().ok_or_else(|| {
                        Error::rejected("assistant proposal endpoint ancestry is empty")
                    })?;
                    let endpoint_session = crate::peer::proc_session(root).map_err(|_| {
                        Error::rejected("assistant proposal endpoint session is unreadable")
                    })?;
                    proof.lane == caller
                        && (caller_session == endpoint_session
                            || self.pi_bash_tool_session(&caller, &proof, caller_session)?)
                }
                Some(SlotWho::Pane { lane, .. }) => {
                    if lane != caller {
                        false
                    } else {
                        let row = self.store.agent(&caller)?;
                        match row.pid.and_then(|pid| u32::try_from(pid).ok()) {
                            Some(pane) => {
                                crate::peer::proc_session(pane).ok() == Some(caller_session)
                            }
                            None => false,
                        }
                    }
                }
                None => false,
            };
            if !inside {
                return Err(Error::rejected(
                    "detached child is outside the assigned agent endpoint session",
                ));
            }
        }
        let install = required_str(params, "install_id")?;
        crate::proto::identifier(install, "installation ID")?;
        let context = required_str(params, "context_id")?;
        let campaign = required_str(params, "campaign_id")?;
        let proposal = required_str(params, "proposal_id")?;
        let message_id = required_str(params, "message")?;
        let token = required_str(params, "token")?;
        if message_id.is_empty() || message_id.len() > 128 {
            return Err(Error::rejected("proposal message identity is malformed"));
        }
        if token.is_empty() || token.len() > 256 {
            return Err(Error::rejected("proposal turn token is malformed"));
        }
        // The live assigned turn, from the daemon's own rows: the
        // message addresses the caller, is running under exactly this
        // token, and the token is current under the endpoint's own
        // scheme and live generation — a token for another turn, a
        // stale generation, or an endpoint with no checkable scheme
        // refuses here.
        let stored = self
            .store
            .message(message_id)?
            .ok_or_else(|| Error::rejected("assistant proposal turn is unknown"))?;
        if stored.alias != caller {
            return Err(Error::rejected(
                "assistant proposal turn belongs to another agent",
            ));
        }
        if stored.state != "running" || stored.turn_id.as_deref() != Some(token) {
            return Err(Error::rejected(
                "assistant proposal needs the active assigned chat turn",
            ));
        }
        let agent = self.store.agent(&caller)?;
        if !registry::turn_token_current(
            &agent.provider,
            &agent.endpoint_kind,
            agent.generation.as_deref(),
            token,
        ) {
            return Err(Error::rejected(
                "assistant proposal turn token is no longer current",
            ));
        }
        // The turn's server-verified App binding, re-proved against
        // the live store: the stamp names exactly this installation
        // and context, or the call has no verified chat scope — a
        // browser value, a turn from another install/context, or a
        // binding revised or archived after send refuses here.
        let hint = self.store.message_app(message_id)?.ok_or_else(|| {
            Error::rejected("assistant proposal turn carries no verified App scope")
        })?;
        if hint.get("install_id").and_then(Value::as_str) != Some(install)
            || hint.get("context_id").and_then(Value::as_str) != Some(context)
        {
            return Err(Error::rejected(
                "assistant proposal scope does not match its verified chat turn",
            ));
        }
        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_runtime_snapshot(&pm, install, |_, _| {
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            self.store.app_context_proof(install, context)?;
            Ok(())
        })?;
        let draft = content_draft(params)?;
        let request_id = content_request_id(params)?;
        let records = RecordStore::open(&self.state_dir, install)?;
        let claim = crate::store::app_content::AssistantClaim {
            agent: &caller,
            message: message_id,
            request: request_id,
        };
        let result =
            records.app_content_assistant_propose(context, campaign, proposal, &draft, &claim)?;
        let digest = result
            .get("proposal")
            .and_then(|proposal| proposal.get("content_digest"))
            .and_then(Value::as_str)
            .unwrap_or("");
        self.store.note_app_content_by(
            install,
            context,
            "app_content_assistant_propose",
            digest,
            &caller,
        );
        self.wake();
        Ok(result)
    }
}
