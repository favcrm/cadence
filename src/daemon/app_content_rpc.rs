//! Strict operator management of versioned campaign email content (CAD-782).
//!
//! Every method first proves the operator connection, then resolves
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
//! the host's own fixed template. Proposals submitted here are
//! recorded `actor='operator'`: the connection proves the operator
//! submitted the draft, never that an assistant produced it —
//! `assistant` attribution stays reserved for CAD-784
//! receipt-backed writes. Sender material renders only through
//! typed preview-only bindings; final-send preparation always refuses
//! until CAD-785/786 supply host-verified evidence. There is no agent-origin
//! edit/approve/send path: proposals, Apply, Discard, approval and
//! send preparation all require the operator connection, so an agent
//! caller or detached child is refused without mutation. The board
//! peer lives in `src/ui/app_content.rs` under this ticket; it
//! follows the CAD-768 strict-peer contract (URL IDs are authority,
//! exact transport grammar, POST-only writes).
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
}
