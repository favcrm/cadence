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
use super::app_audiences_rpc::{audience_expected, audience_name, audience_predicates};
use super::app_records_rpc::{csv_decisions, csv_text};
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

/// CAD-1056: the operator save body is exactly one of `blocks`
/// (structured) or `html` (pasted, host-sanitised), plus an optional
/// plain-text override. Proposals keep the blocks-only `content_draft`.
fn content_save_draft(params: &Value) -> Result<Draft> {
    let subject = required_str(params, "subject")?;
    let preheader = match params.get("preheader") {
        None => "",
        Some(Value::String(text)) => text.as_str(),
        Some(_) => return Err(Error::rejected("email preheader must be a string")),
    };
    let text = match params.get("text") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.as_str()),
        Some(_) => return Err(Error::rejected("email plain text must be a string")),
    };
    let draft = match (params.get("blocks"), params.get("html")) {
        (Some(_), Some(_)) | (None, None) => {
            return Err(Error::rejected(
                "email content needs exactly one of blocks or html",
            ));
        }
        (Some(_), None) => content_draft(params)?,
        (None, Some(Value::String(html))) => Draft::parse_html(subject, preheader, html)
            .map_err(|_| Error::rejected("email HTML exceeds its supported shape or bounds"))?,
        (None, Some(_)) => return Err(Error::rejected("email HTML must be a string")),
    };
    let name = match params.get("name") {
        None | Some(Value::Null) => None,
        Some(Value::String(name)) => Some(name.as_str()),
        Some(_) => return Err(Error::rejected("campaign name must be a string")),
    };
    draft.with_text(text)?.with_name(name)
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
                "name",
                "subject",
                "preheader",
                "blocks",
                "html",
                "text",
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
            "app_content_proposal_render" => &[
                "install_id",
                "context_id",
                "proposal_id",
                "sample_first_name",
                "binding_id",
            ],
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
                | "app_content_proposal_render"
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
            "app_content_save" => {
                let draft = content_save_draft(params)?;
                if let Some(html) = &draft.html {
                    // The host's own unsubscribe URL shapes: saved
                    // bindings, the preview base and the configured
                    // send origin (`{origin}/unsubscribe/<token>`).
                    let mut endpoints = records.app_unsubscribe_endpoints(context)?;
                    if let Ok(origin) = self.crm_send_origin() {
                        endpoints.extend(crate::store::app_content_html::HostEndpoint::origin(
                            &origin,
                        ));
                    }
                    crate::store::app_content_html::refuse_host_unsubscribe(html, &endpoints)?;
                }
                records.app_content_save(
                    context,
                    required_str(params, "campaign_id")?,
                    content_expected(params)?,
                    &draft,
                )
            }
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
            // CAD-1014: the operator's before-Apply preview of a pending
            // proposal — the SAME safe render_html/text the saved-content
            // path uses, over the stored proposal draft. Pure read; no
            // apply/save/approve/send, send_ready always false.
            "app_content_proposal_render" => {
                let (_, sample) = content_render_scope(params)?;
                records.app_content_proposal_render(
                    context,
                    required_str(params, "proposal_id")?,
                    sample.as_deref(),
                    content_binding(params)?,
                )
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
        let ScopedChat {
            caller,
            install,
            context,
            message_id,
            ..
        } = self.scoped_chat_assistant(params, peer_pid, "assistant proposal")?;
        let campaign = required_str(params, "campaign_id")?;
        let proposal = required_str(params, "proposal_id")?;
        let draft = content_draft(params)?;
        let request_id = content_request_id(params)?;
        let records = RecordStore::open(&self.state_dir, &install)?;
        let claim = crate::store::app_content::AssistantClaim {
            agent: &caller,
            message: &message_id,
            request: request_id,
        };
        let result =
            records.app_content_assistant_propose(&context, campaign, proposal, &draft, &claim)?;
        let digest = result
            .get("proposal")
            .and_then(|proposal| proposal.get("content_digest"))
            .and_then(Value::as_str)
            .unwrap_or("");
        self.store.note_app_content_by(
            &install,
            &context,
            "app_content_assistant_propose",
            digest,
            &caller,
        );
        self.wake();
        Ok(result)
    }

    /// CAD-1014(b): a scoped chat turn's delegated customer CSV import.
    /// The intent is the OPERATOR's own genuine scoped chat message
    /// (`thread_send` carrying the daemon-verified App binding), not a
    /// separate mint verb: [`Self::scoped_chat_assistant`] proves the
    /// caller is a connection-derived agent on the live assigned turn
    /// whose `message_app` stamp names exactly this install+context, and
    /// that the caller lives in the endpoint's own session (a detached
    /// child is refused). The exact CSV bytes are still bound by the
    /// existing `preview_token` (a `sha256:` of the very bytes the
    /// operator previewed) and `request_id` is the idempotency key — the
    /// user-facing explicit-confirm gate lives in the chat surface,
    /// which the agent cannot reach; an agent can only commit the bytes
    /// the operator previewed. Agent-supplied `install_id`/`context_id`
    /// must EQUAL the stamped scope, never widen it. This verb can never
    /// send, approve or touch a record outside the stamped context.
    pub(super) fn rpc_app_record_csv_assistant_import(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let fields = params
            .as_object()
            .ok_or_else(|| Error::rejected("app record payload must be an object"))?;
        const ALLOWED: &[&str] = &[
            "install_id",
            "context_id",
            "request_id",
            "confirm_token",
            "message",
            "token",
        ];
        if fields.keys().any(|key| !ALLOWED.contains(&key.as_str())) {
            return Err(Error::rejected("app record payload has unsupported fields"));
        }
        let scoped = self.scoped_chat_assistant(params, peer_pid, "assistant CSV import")?;
        let records = RecordStore::open(&self.state_dir, &scoped.install)?;
        let request_id = required_str(params, "request_id")?;
        let confirm_token = required_str(params, "confirm_token")?;
        // The durable server-held plan IS the intent: the agent names
        // only request_id + the operator's one-use nonce; the host
        // resolves the confirmed csv_text + decisions + their digests
        // from the confirm row. Bytes/decisions/preview_token are never
        // on the wire — the agent can never substitute a plan the
        // operator did not confirm (CSV runs to 256KiB, the chat ≤48KB;
        // bytes can't and don't ride the message).
        let (csv_text, decisions_value, decisions_digest, preview_token) =
            records.app_record_csv_confirm_plan(&scoped.context, request_id, confirm_token)?;
        // One stamped message redeems one action across all request ids
        // (CAD-1014). The claim binds the resolved plan handle (request
        // id + the plan's own digests) so a changed plan under a spent
        // claim refuses as a second intent, never a replay.
        let payload_digest = crate::store::app_runs::material_digest(&json!({
            "domain": "cadence-app-csv-assistant-import-v1",
            "request_id": request_id,
            "preview_token": preview_token,
            "decisions_digest": decisions_digest,
        }));
        records.app_assistant_claim(
            &scoped.context,
            &scoped.message_id,
            "app_record_csv_assistant_import",
            Some(request_id),
            &payload_digest,
            &scoped.caller,
        )?;
        // Reuse the strict decisions parser over the stored array.
        let decisions = csv_decisions(&json!({"decisions": decisions_value}))?;
        // Identical replay (same request id + bytes + confirmed digest,
        // already completed) returns the stored receipt WITHOUT spending
        // a confirm — the claim bound this exact plan.
        let already = records.app_record_csv_receipt_exists(
            &scoped.context,
            request_id,
            &preview_token,
            &decisions_digest,
        )?;
        // The operator's confirm is spent atomically inside the import's
        // own reservation transaction — a crash can never burn it
        // without a pending receipt. A replay needs no confirm (the
        // receipt IS the receipt); a fresh import spends it here.
        let confirm = if already {
            None
        } else {
            Some((confirm_token, decisions_digest.as_str()))
        };
        let result = records.app_record_csv_import(
            &scoped.context,
            &csv_text,
            &preview_token,
            request_id,
            decisions,
            confirm,
        )?;
        self.store.note_app_record_csv_import(
            &scoped.install,
            &scoped.context,
            required_str(params, "request_id").unwrap_or(""),
            result["summary"]["applied"].as_i64().unwrap_or(0),
            result["summary"]["skipped"].as_i64().unwrap_or(0),
            result["summary"]["failed"].as_i64().unwrap_or(0),
        );
        self.wake();
        Ok(result)
    }

    /// CAD-1014(b): a scoped chat turn's delegated segment save. Same
    /// intent source and redeem gate as the CSV import; the segment id
    /// stays agent-chosen inside the stamped scope and `expected_revision`
    /// is the CAS the operator path already uses, so an agent can create
    /// or revise only the segment it names inside the verified context —
    /// never a blanket write. No send/approve.
    pub(super) fn rpc_app_segment_assistant_save(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let fields = params
            .as_object()
            .ok_or_else(|| Error::rejected("app audience payload must be an object"))?;
        const ALLOWED: &[&str] = &[
            "install_id",
            "context_id",
            "segment_id",
            "name",
            "predicates",
            "expected_revision",
            "message",
            "token",
        ];
        if fields.keys().any(|key| !ALLOWED.contains(&key.as_str())) {
            return Err(Error::rejected(
                "app audience payload has unsupported fields",
            ));
        }
        let scoped = self.scoped_chat_assistant(params, peer_pid, "assistant segment save")?;
        let records = RecordStore::open(&self.state_dir, &scoped.install)?;
        // One stamped message redeems one action across all ids
        // (CAD-1014); `expected_revision` is the CAS on the named
        // segment. The claim binds the normalized segment payload — a
        // re-save of the SAME segment replays, a same-id segment with a
        // changed name/predicates/revision refuses as a different intent.
        let payload_digest = crate::store::app_runs::material_digest(&json!({
            "domain": "cadence-app-segment-assistant-save-v1",
            "segment_id": required_str(params, "segment_id")?,
            "name": required_str(params, "name")?,
            "predicates": params.get("predicates").cloned().unwrap_or(Value::Null),
            "expected_revision": params.get("expected_revision").cloned().unwrap_or(Value::Null),
        }));
        records.app_assistant_claim(
            &scoped.context,
            &scoped.message_id,
            "app_segment_assistant_save",
            Some(required_str(params, "segment_id")?),
            &payload_digest,
            &scoped.caller,
        )?;
        let result = records.app_segment_save(
            &scoped.context,
            required_str(params, "segment_id")?,
            audience_expected(params)?,
            &audience_name(params)?,
            &audience_predicates(params)?,
        )?;
        let digest = result
            .get("segment")
            .and_then(|segment| segment.get("digest"))
            .and_then(Value::as_str)
            .unwrap_or("");
        self.store.note_app_audience(
            &scoped.install,
            &scoped.context,
            "app_segment_assistant_save",
            digest,
        );
        self.wake();
        Ok(result)
    }

    /// CAD-1014(b): scoped-chat read/preview verbs — the agent's read of
    /// the stamped install/context. Same redeem gate (connection-derived
    /// agent, live turn, re-proved scope) but NO claim: reads and
    /// inert previews do not consume the message. Agent-supplied
    /// install/context must equal the stamp; a foreign scope refuses.
    pub(super) fn rpc_app_assistant_read(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let fields = params
            .as_object()
            .ok_or_else(|| Error::rejected("app assistant read payload must be an object"))?;
        const ALLOWED: &[&str] = &[
            "install_id",
            "context_id",
            "segment_id",
            "campaign_id",
            "proposal_id",
            "limit",
            "cursor",
            "csv_text",
            "message",
            "token",
        ];
        if fields.keys().any(|key| !ALLOWED.contains(&key.as_str())) {
            return Err(Error::rejected(
                "app assistant read payload has unsupported fields",
            ));
        }
        let scoped = self.scoped_chat_assistant(params, peer_pid, "assistant read")?;
        let records = RecordStore::open(&self.state_dir, &scoped.install)?;
        // Reads only — no claim, no mutation, no audit write.
        match method {
            "app_segment_assistant_list" => records.app_segment_list(&scoped.context),
            "app_segment_assistant_show" => {
                records.app_segment_show(&scoped.context, required_str(params, "segment_id")?)
            }
            "app_record_csv_assistant_preview" => {
                records.app_record_csv_preview(&scoped.context, &csv_text(params)?)
            }
            // A bounded membership preview over a SAVED segment —
            // counts + bounded sample, never the full member list,
            // never a freeze or send (root's required segment-preview
            // acceptance).
            "app_segment_assistant_preview" => {
                records.app_segment_preview(&scoped.context, required_str(params, "segment_id")?)
            }
            // The agent's inert pending draft must be discoverable in
            // the campaign's proposal list BEFORE the operator applies
            // it — list by campaign (optionally) and show one proposal.
            "app_content_assistant_proposals" => records.app_content_proposal_list(
                &scoped.context,
                params.get("campaign_id").and_then(Value::as_str),
            ),
            "app_content_assistant_proposal_show" => records
                .app_content_proposal_show(&scoped.context, required_str(params, "proposal_id")?),
            _ => Err(Error::rejected("unknown app assistant read method")),
        }
    }

    /// CAD-1014(b) composer-free email draft: the agent turn drafts a
    /// campaign email straight from the scoped chat — NO operator mint,
    /// no request id. The host derives campaign source revision (0 for a
    /// first draft) and stamps the proposal `pending` /
    /// `assistant-receipt` with the message id as the receipt — the
    /// verified turn IS the intent. The unique per-message claim makes
    /// one turn produce one draft; an identical proposal id + bytes +
    /// same message replays idempotently. Never edits live content,
    /// approves or sends — Apply/Discard stay the operator's.
    pub(super) fn rpc_app_content_assistant_draft(
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
        ];
        if fields.keys().any(|key| !ALLOWED.contains(&key.as_str())) {
            return Err(Error::rejected(
                "app content payload has unsupported fields",
            ));
        }
        let scoped = self.scoped_chat_assistant(params, peer_pid, "assistant email draft")?;
        let draft = content_draft(params)?;
        let records = RecordStore::open(&self.state_dir, &scoped.install)?;
        let result = records.app_content_assistant_draft(
            &scoped.context,
            required_str(params, "campaign_id")?,
            required_str(params, "proposal_id")?,
            &draft,
            &scoped.caller,
            &scoped.message_id,
        )?;
        let digest = result
            .get("proposal")
            .and_then(|proposal| proposal.get("content_digest"))
            .and_then(Value::as_str)
            .unwrap_or("");
        self.store.note_app_content_by(
            &scoped.install,
            &scoped.context,
            "app_content_assistant_draft",
            digest,
            &scoped.caller,
        );
        self.wake();
        Ok(result)
    }
}

/// The verified scoped-chat redeem context a delegated assistant verb
/// runs under (CAD-1014). The caller is the connection-derived agent;
/// install+context come from the re-proved `message_app` stamp on the
/// operator's own scoped chat message — never agent text.
pub(super) struct ScopedChat {
    pub(super) caller: String,
    pub(super) install: String,
    pub(super) context: String,
    pub(super) message_id: String,
}

impl Shared {
    /// The shared CAD-813/CAD-1014(b) scoped-chat redeem gate. `desc`
    /// names the verb in refusal text. Returns the connection-derived
    /// caller and the stamped install/context; a detached child, an
    /// unproven caller, the operator, a foreign or stale turn, and any
    /// scope mismatch all refuse before any file opens.
    pub(super) fn scoped_chat_assistant(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
        desc: &str,
    ) -> Result<ScopedChat> {
        // The caller is the connection's alone: identity-shaped,
        // receipt-shaped and routing fields are not transport fields
        // here at all — the allowlist above already refused them —
        // and the agent is never named, only derived.
        let caller = match self.connection_caller(peer_pid)? {
            caller_rule::Who::Agent(alias) => alias,
            caller_rule::Who::Operator => {
                return Err(Error::rejected(format!(
                    "{desc} is an agent turn's verb — the operator acts through the app record, segment and content verbs"
                )));
            }
            caller_rule::Who::Unproven(why) => {
                return Err(Error::rejected(format!(
                    "{desc} refused: this connection derives no agent identity and is \
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
                .map_err(|_| Error::rejected(format!("{desc} caller session is unreadable")))?;
            let inside = match self.slot_identity(peer_pid)? {
                Some(SlotWho::Strict(proof)) => {
                    let root = *proof.segment.last().ok_or_else(|| {
                        Error::rejected(format!("{desc} endpoint ancestry is empty"))
                    })?;
                    let endpoint_session = crate::peer::proc_session(root).map_err(|_| {
                        Error::rejected(format!("{desc} endpoint session is unreadable"))
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
        let message_id = required_str(params, "message")?;
        let token = required_str(params, "token")?;
        if message_id.is_empty() || message_id.len() > 128 {
            return Err(Error::rejected(format!(
                "{desc} message identity is malformed"
            )));
        }
        if token.is_empty() || token.len() > 256 {
            return Err(Error::rejected(format!("{desc} turn token is malformed")));
        }
        // The live assigned turn, from the daemon's own rows: the
        // message addresses the caller, is running under exactly this
        // token, and the token is current under the endpoint's own
        // scheme and live generation.
        let stored = self
            .store
            .message(message_id)?
            .ok_or_else(|| Error::rejected(format!("{desc} turn is unknown")))?;
        if stored.alias != caller {
            return Err(Error::rejected(format!(
                "{desc} turn belongs to another agent"
            )));
        }
        if stored.state != "running" || stored.turn_id.as_deref() != Some(token) {
            return Err(Error::rejected(format!(
                "{desc} needs the active assigned chat turn"
            )));
        }
        let agent = self.store.agent(&caller)?;
        if !registry::turn_token_current(
            &agent.provider,
            &agent.endpoint_kind,
            agent.generation.as_deref(),
            token,
        ) {
            return Err(Error::rejected(format!(
                "{desc} turn token is no longer current"
            )));
        }
        // The turn's server-verified App binding, re-proved against
        // the live store: the stamp names exactly this installation
        // and context, or the call has no verified chat scope.
        let hint = self
            .store
            .message_app(message_id)?
            .ok_or_else(|| Error::rejected(format!("{desc} turn carries no verified App scope")))?;
        if hint.get("install_id").and_then(Value::as_str) != Some(install)
            || hint.get("context_id").and_then(Value::as_str) != Some(context)
        {
            return Err(Error::rejected(format!(
                "{desc} scope does not match its verified chat turn"
            )));
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
        Ok(ScopedChat {
            caller,
            install: install.to_string(),
            context: context.to_string(),
            message_id: message_id.to_string(),
        })
    }
}
