//! Versioned campaign email content, sender bindings and operator
//! proposals; every route requires operator proof (CAD-782).
//!
//! The board relays the daemon's `app_content_*` and
//! `app_sender_binding_*` actions with the URL's installation,
//! context, campaign, proposal and binding IDs as authority — body
//! fields can never claim identity (`by`, `actor`), receipt
//! (`assistant_receipt`, `turn_id`, `nonce`), routing (`workspace`)
//! or discovery links (`project`, `project_link`), nor smuggle the
//! URL IDs themselves. Subjects, blocks, tokens, button URLs and
//! sender material are forwarded as typed JSON for the daemon's
//! allowlisted grammar; no HTML is ever built here. Errors are
//! bounded generics that never echo content; renders stay bounded
//! server-side, and full customer data never leaves the daemon at
//! all.
use super::{err_response, json_response, read_body, HttpResp};
use crate::{client, error::Error};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use tiny_http::{Header, Request};

const BODY_CAP: u64 = 48 * 1024;
const RESULT_CAP: usize = 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) enum Route<'a> {
    CampaignSave(&'a str, &'a str),
    CampaignList(&'a str, &'a str),
    CampaignShow(&'a str, &'a str, &'a str),
    Render(&'a str, &'a str, &'a str),
    Approve(&'a str, &'a str, &'a str),
    TestPrepare(&'a str, &'a str, &'a str),
    SendPrepare(&'a str, &'a str, &'a str),
    ProposalPropose(&'a str, &'a str),
    ProposalRequest(&'a str, &'a str),
    ProposalList(&'a str, &'a str),
    ProposalShow(&'a str, &'a str, &'a str),
    ProposalApply(&'a str, &'a str, &'a str),
    ProposalDiscard(&'a str, &'a str, &'a str),
    BindingSave(&'a str, &'a str),
    BindingList(&'a str, &'a str),
    BindingShow(&'a str, &'a str, &'a str),
}

fn segment(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

pub(super) fn route(path: &str) -> Option<Route<'_>> {
    let tail = path.strip_prefix("/api/app-installations/")?;
    let mut parts = tail.split('/');
    let install = parts.next()?;
    if !segment(install) || parts.next()? != "contexts" {
        return None;
    }
    let context = parts.next()?;
    if !segment(context) || parts.next()? != "content" {
        return None;
    }
    let section = parts.next()?;
    let rest: Vec<&str> = parts.collect();
    // Reserved verbs precede IDs: `list` under campaigns/proposals
    // and the suffixed verbs under a campaign or proposal ID are
    // unaddressable as IDs over HTTP (RPC still serves them), so a
    // bulk POST can never be mistaken for a single-row read, matching
    // the segments peer's reservation. Saves ride the collection
    // path with the campaign/proposal ID in the body — the daemon
    // validates its identifier grammar — while install/context stay
    // URL authority throughout.
    match (section, rest.as_slice()) {
        ("campaigns", []) => Some(Route::CampaignSave(install, context)),
        ("campaigns", ["list"]) => Some(Route::CampaignList(install, context)),
        ("campaigns", [id]) if segment(id) => Some(Route::CampaignShow(install, context, id)),
        ("campaigns", [id, "render"]) if segment(id) => Some(Route::Render(install, context, id)),
        ("campaigns", [id, "approve"]) if segment(id) => Some(Route::Approve(install, context, id)),
        ("campaigns", [id, "test-prepare"]) if segment(id) => {
            Some(Route::TestPrepare(install, context, id))
        }
        ("campaigns", [id, "send-prepare"]) if segment(id) => {
            Some(Route::SendPrepare(install, context, id))
        }
        ("proposals", []) => Some(Route::ProposalPropose(install, context)),
        ("proposal-requests", []) => Some(Route::ProposalRequest(install, context)),
        ("proposals", ["list"]) => Some(Route::ProposalList(install, context)),
        ("proposals", [id]) if segment(id) => Some(Route::ProposalShow(install, context, id)),
        ("proposals", [id, "apply"]) if segment(id) => {
            Some(Route::ProposalApply(install, context, id))
        }
        ("proposals", [id, "discard"]) if segment(id) => {
            Some(Route::ProposalDiscard(install, context, id))
        }
        ("sender-bindings", []) => Some(Route::BindingSave(install, context)),
        ("sender-bindings", ["list"]) => Some(Route::BindingList(install, context)),
        ("sender-bindings", [id]) if segment(id) => Some(Route::BindingShow(install, context, id)),
        _ => None,
    }
}

impl Route<'_> {
    pub(super) fn is_read(self) -> bool {
        matches!(
            self,
            Self::CampaignList(..)
                | Self::CampaignShow(..)
                | Self::ProposalList(..)
                | Self::ProposalShow(..)
                | Self::BindingList(..)
                | Self::BindingShow(..)
        )
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ContentSave {
    campaign_id: String,
    subject: String,
    #[serde(default)]
    preheader: String,
    blocks: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_revision: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ContentRender {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sample_first_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    binding_id: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ContentApprove {
    expected_revision: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ContentTestPrepare {
    to_email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    binding_id: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ContentSendPrepare {
    binding_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    audience_freeze_id: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BindingSave {
    binding_id: String,
    sender_name: String,
    sender_address: String,
    unsubscribe_base: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    connection_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_revision: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProposalPropose {
    campaign_id: String,
    proposal_id: String,
    subject: String,
    #[serde(default)]
    preheader: String,
    blocks: Value,
}

/// CAD-813: the operator mints a one-time proposal request against a
/// chat message. `message_id` names the chat turn; campaign and
/// source revision are host-stamped by the daemon, never supplied.
/// No turn token, receipt or source field rides this body.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProposalRequest {
    campaign_id: String,
    message_id: String,
    request_id: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProposalApply {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_revision: Option<u64>,
}

fn typed<T: serde::de::DeserializeOwned>(request: &mut Request) -> Result<T, HttpResp> {
    let bytes = read_body(request, BODY_CAP)?;
    serde_json::from_slice(&bytes)
        .map_err(|_| err_response(400, "invalid app content request schema"))
}

fn revision(value: u64) -> Result<i64, HttpResp> {
    i64::try_from(value)
        .ok()
        .filter(|revision| *revision > 0)
        .ok_or_else(|| err_response(400, "invalid app content request schema"))
}

pub(super) fn handle(
    request: &mut Request,
    state: &Path,
    route: Route<'_>,
    write: bool,
) -> HttpResp {
    if request
        .url()
        .split_once('?')
        .is_some_and(|(_, q)| !q.is_empty())
    {
        return err_response(400, "app content query parameters are unsupported");
    }
    let (method, params) = match route {
        Route::CampaignList(install, context) if !write => (
            "app_content_list",
            json!({"install_id": install, "context_id": context}),
        ),
        Route::CampaignShow(install, context, id) if !write => (
            "app_content_show",
            json!({"install_id": install, "context_id": context, "campaign_id": id}),
        ),
        // Saves ride the collection path; the daemon validates the
        // body's campaign identifier grammar. Install/context stay
        // URL authority.
        Route::CampaignSave(install, context) => {
            let body: ContentSave = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let mut params = json!({
                "install_id": install, "context_id": context,
                "campaign_id": body.campaign_id,
                "subject": body.subject, "preheader": body.preheader,
                "blocks": body.blocks,
            });
            if let Some(expected) = body.expected_revision {
                params["expected_revision"] = match revision(expected) {
                    Ok(value) => value.into(),
                    Err(response) => return response,
                };
            }
            ("app_content_save", params)
        }
        Route::Render(install, context, id) => {
            let body: ContentRender = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let mut params =
                json!({"install_id": install, "context_id": context, "campaign_id": id});
            if let Some(wanted) = body.revision {
                params["revision"] = match revision(wanted) {
                    Ok(value) => value.into(),
                    Err(response) => return response,
                };
            }
            if let Some(sample) = body.sample_first_name {
                params["sample_first_name"] = Value::String(sample);
            }
            if let Some(binding) = body.binding_id {
                params["binding_id"] = Value::String(binding);
            }
            ("app_content_render", params)
        }
        Route::Approve(install, context, id) => {
            let body: ContentApprove = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let expected = match revision(body.expected_revision) {
                Ok(value) => value,
                Err(response) => return response,
            };
            (
                "app_content_approve",
                json!({"install_id": install, "context_id": context, "campaign_id": id, "expected_revision": expected}),
            )
        }
        Route::TestPrepare(install, context, id) => {
            let body: ContentTestPrepare = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let mut params = json!({"install_id": install, "context_id": context, "campaign_id": id, "to_email": body.to_email});
            if let Some(binding) = body.binding_id {
                params["binding_id"] = Value::String(binding);
            }
            ("app_content_test_prepare", params)
        }
        Route::SendPrepare(install, context, id) => {
            let body: ContentSendPrepare = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let mut params = json!({"install_id": install, "context_id": context, "campaign_id": id, "binding_id": body.binding_id});
            if let Some(freeze) = body.audience_freeze_id {
                params["audience_freeze_id"] = Value::String(freeze);
            }
            ("app_content_send_prepare", params)
        }
        Route::BindingSave(install, context) => {
            let body: BindingSave = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let mut params = json!({
                "install_id": install, "context_id": context,
                "binding_id": body.binding_id, "sender_name": body.sender_name,
                "sender_address": body.sender_address,
                "unsubscribe_base": body.unsubscribe_base,
            });
            if let Some(connection) = body.connection_id {
                params["connection_id"] = Value::String(connection);
            }
            if let Some(expected) = body.expected_revision {
                params["expected_revision"] = match revision(expected) {
                    Ok(value) => value.into(),
                    Err(response) => return response,
                };
            }
            ("app_sender_binding_save", params)
        }
        Route::BindingList(install, context) if !write => (
            "app_sender_binding_list",
            json!({"install_id": install, "context_id": context}),
        ),
        Route::BindingShow(install, context, id) if !write => (
            "app_sender_binding_show",
            json!({"install_id": install, "context_id": context, "binding_id": id}),
        ),
        Route::ProposalPropose(install, context) => {
            let body: ProposalPropose = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            (
                "app_content_propose",
                json!({"install_id": install, "context_id": context,
                    "campaign_id": body.campaign_id, "proposal_id": body.proposal_id,
                    "subject": body.subject, "preheader": body.preheader,
                    "blocks": body.blocks}),
            )
        }
        // CAD-813: the operator's one-time request mint. The daemon
        // stamps campaign scope and source revision against the live
        // installation/context/content and the message's verified App
        // binding; the typed body names only the chat message.
        Route::ProposalRequest(install, context) => {
            let body: ProposalRequest = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            (
                "app_content_proposal_request",
                json!({"install_id": install, "context_id": context,
                    "campaign_id": body.campaign_id, "message": body.message_id,
                    "request_id": body.request_id}),
            )
        }
        // The list carries no filter over HTTP: campaign scoping
        // stays a daemon RPC option, and any body refuses so a
        // filtered read cannot smuggle parameters past the URL.
        Route::ProposalList(install, context) if !write => match read_body(request, BODY_CAP) {
            Ok(bytes) if bytes.is_empty() => (
                "app_content_proposal_list",
                json!({"install_id": install, "context_id": context}),
            ),
            Ok(_) => return err_response(400, "invalid app content request schema"),
            Err(response) => return response,
        },
        Route::ProposalShow(install, context, id) if !write => (
            "app_content_proposal_show",
            json!({"install_id": install, "context_id": context, "proposal_id": id}),
        ),
        Route::ProposalApply(install, context, id) => {
            let bytes = match read_body(request, BODY_CAP) {
                Ok(bytes) => bytes,
                Err(response) => return response,
            };
            let mut params =
                json!({"install_id": install, "context_id": context, "proposal_id": id});
            if !bytes.is_empty() {
                let body: ProposalApply = match serde_json::from_slice(&bytes) {
                    Ok(body) => body,
                    Err(_) => {
                        return err_response(400, "invalid app content request schema");
                    }
                };
                if let Some(expected) = body.expected_revision {
                    params["expected_revision"] = match revision(expected) {
                        Ok(value) => value.into(),
                        Err(response) => return response,
                    };
                }
            }
            ("app_content_proposal_apply", params)
        }
        // Discard takes no parameters beyond the URL IDs; any body
        // refuses so the transport cannot smuggle fields.
        Route::ProposalDiscard(install, context, id) => match read_body(request, BODY_CAP) {
            Ok(bytes) if bytes.is_empty() => (
                "app_content_proposal_discard",
                json!({"install_id": install, "context_id": context, "proposal_id": id}),
            ),
            Ok(_) => return err_response(400, "invalid app content request schema"),
            Err(response) => return response,
        },
        // Reads served on the write transport are refused: the
        // read peer serves them after operator-read admission.
        _ => return err_response(405, "method not allowed"),
    };
    match client::rpc(state, method, params) {
        Ok(value) if value.to_string().len() <= RESULT_CAP => {
            let mut response = json_response(value);
            response.add_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
            response
        }
        Ok(_) => err_response(502, "app content receipt exceeds bound"),
        Err(error) => {
            let denied = error.to_string().contains("operator action")
                || error.to_string().contains("not provably the operator");
            let code = if denied {
                403
            } else {
                match error {
                    Error::Rejected(_) | Error::Structured(_) => 409,
                    _ => 503,
                }
            };
            err_response(code, "app content management refused or unavailable")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routes_and_body_schema_are_exact() {
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/content/campaigns"),
            Some(Route::CampaignSave("i", "c"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/content/campaigns/launch-1"),
            Some(Route::CampaignShow("i", "c", "launch-1"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/content/campaigns/list"),
            Some(Route::CampaignList("i", "c"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/content/campaigns/launch-1/render"),
            Some(Route::Render("i", "c", "launch-1"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/content/campaigns/launch-1/approve"),
            Some(Route::Approve("i", "c", "launch-1"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/content/campaigns/launch-1/test-prepare"),
            Some(Route::TestPrepare("i", "c", "launch-1"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/content/campaigns/launch-1/send-prepare"),
            Some(Route::SendPrepare("i", "c", "launch-1"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/content/proposals"),
            Some(Route::ProposalPropose("i", "c"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/content/proposal-requests"),
            Some(Route::ProposalRequest("i", "c"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/content/proposals/list"),
            Some(Route::ProposalList("i", "c"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/content/proposals/prop-1"),
            Some(Route::ProposalShow("i", "c", "prop-1"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/content/proposals/prop-1/apply"),
            Some(Route::ProposalApply("i", "c", "prop-1"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/content/proposals/prop-1/discard"),
            Some(Route::ProposalDiscard("i", "c", "prop-1"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/content/sender-bindings"),
            Some(Route::BindingSave("i", "c"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/content/sender-bindings/list"),
            Some(Route::BindingList("i", "c"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/content/sender-bindings/bind-1"),
            Some(Route::BindingShow("i", "c", "bind-1"))
        ));
        // Collection saves and verb routes are writes; list/show are reads.
        assert!(
            !route("/api/app-installations/i/contexts/c/content/campaigns")
                .unwrap()
                .is_read()
        );
        assert!(
            route("/api/app-installations/i/contexts/c/content/campaigns/launch-1")
                .unwrap()
                .is_read()
        );
        assert!(
            !route("/api/app-installations/i/contexts/c/content/campaigns/launch-1/render")
                .unwrap()
                .is_read()
        );
        assert!(
            route("/api/app-installations/i/contexts/c/content/campaigns/list")
                .unwrap()
                .is_read()
        );
        assert!(
            route("/api/app-installations/i/contexts/c/content/proposals/list")
                .unwrap()
                .is_read()
        );
        // The request mint is a write like the proposal save.
        assert!(
            !route("/api/app-installations/i/contexts/c/content/proposal-requests")
                .unwrap()
                .is_read()
        );
        assert!(
            !route("/api/app-installations/i/contexts/c/content/sender-bindings")
                .unwrap()
                .is_read()
        );
        assert!(
            route("/api/app-installations/i/contexts/c/content/sender-bindings/list")
                .unwrap()
                .is_read()
        );
        for path in [
            "/api/app-installations/i/contexts/c/content",
            "/api/app-installations/i/contexts/c/content/campaigns/",
            "/api/app-installations/i/contexts/c/content/campaigns/list/extra",
            "/api/app-installations/i/contexts/c/content/campaigns/launch-1/extra",
            "/api/app-installations/i/contexts/c/content/campaigns/launch-1/render/extra",
            "/api/app-installations/i/contexts/c/content/proposals/prop-1/extra",
            "/api/app-installations/i/contexts/c/content/proposals/prop-1/apply/extra",
            "/api/app-installations/i/contexts/c/content/sender-bindings/list/extra",
            "/api/app-installations/i/contexts/c/content/sender-bindings/bind-1/extra",
            "/api/app-installations/i/contexts/c/segments",
            "/api/app-installations/../contexts",
        ] {
            assert!(route(path).is_none(), "route admitted {path}");
        }
        // Save bodies carry no identity: URL segments are the
        // authority, and every extra field refuses at the transport.
        assert!(serde_json::from_str::<ContentSave>(
            r#"{"campaign_id":"launch-1","subject":"Hi","blocks":[]}"#
        )
        .is_ok());
        for body in [
            r#"{"campaign_id":"launch-1","subject":"Hi","blocks":[],"install_id":"other"}"#,
            r#"{"campaign_id":"launch-1","subject":"Hi","blocks":[],"by":"operator"}"#,
            r#"{"campaign_id":"launch-1","subject":"Hi","blocks":[],"project":"client"}"#,
            r#"{"campaign_id":"launch-1","subject":"Hi"}"#,
            r#"{"campaign_id":"launch-1","subject":"Hi","blocks":[],"actor":"op"}"#,
        ] {
            assert!(
                serde_json::from_str::<ContentSave>(body).is_err(),
                "content save admitted {body}"
            );
        }
        assert!(serde_json::from_str::<ProposalPropose>(
            r#"{"campaign_id":"launch-1","proposal_id":"prop-1","subject":"Hi","blocks":[]}"#
        )
        .is_ok());
        assert!(serde_json::from_str::<ProposalPropose>(
            r#"{"campaign_id":"launch-1","proposal_id":"prop-1","subject":"Hi","blocks":[],"workspace":"w"}"#
        )
        .is_err());
        assert!(serde_json::from_str::<ContentApprove>(r#"{"expected_revision":2}"#).is_ok());
        assert!(serde_json::from_str::<ContentApprove>(r#"{"expected_revision":0}"#).is_ok());
        assert!(
            serde_json::from_str::<ContentTestPrepare>(r#"{"to_email":"op@example.com"}"#).is_ok()
        );
        assert!(serde_json::from_str::<ContentTestPrepare>(
            r#"{"to_email":"op@example.com","actor":"op"}"#
        )
        .is_err());
        // Send preparation requires the binding: the transport
        // refuses a binding-less body before the daemon is reached.
        assert!(serde_json::from_str::<ContentSendPrepare>(r#"{"binding_id":"bind-1"}"#).is_ok());
        assert!(serde_json::from_str::<ContentSendPrepare>(r#"{}"#).is_err());
        assert!(serde_json::from_str::<ContentSendPrepare>(
            r#"{"binding_id":"bind-1","actor":"op"}"#
        )
        .is_err());
        // Receipt-shaped fields never ride any content body.
        // The request mint names only the chat message: campaign
        // and source are host-stamped, so token/source/receipt
        // fields refuse here.
        assert!(serde_json::from_str::<ProposalRequest>(
            r#"{"campaign_id":"launch-1","message_id":"m-1","request_id":"req-1"}"#
        )
        .is_ok());
        for body in [
            r#"{"campaign_id":"launch-1","message_id":"m-1","request_id":"req-1","token":"t-1"}"#,
            r#"{"campaign_id":"launch-1","message_id":"m-1","request_id":"req-1","source_revision":1}"#,
            r#"{"campaign_id":"launch-1","message_id":"m-1","request_id":"req-1","assistant_receipt":{"turn_id":"t"}}"#,
            r#"{"campaign_id":"launch-1","message_id":"m-1","request_id":"req-1","actor":"op"}"#,
            r#"{"campaign_id":"launch-1","message_id":"m-1"}"#,
            r#"{"campaign_id":"launch-1","message_id":"m-1","request_id":"req-1","install_id":"other"}"#,
        ] {
            assert!(
                serde_json::from_str::<ProposalRequest>(body).is_err(),
                "request mint admitted {body}"
            );
        }
        assert!(serde_json::from_str::<ProposalPropose>(
            r#"{"campaign_id":"launch-1","proposal_id":"prop-1","subject":"Hi","blocks":[],"assistant_receipt":{"turn_id":"t"}}"#
        )
        .is_err());
        assert!(serde_json::from_str::<ContentSave>(
            r#"{"campaign_id":"launch-1","subject":"Hi","blocks":[],"turn_id":"t"}"#
        )
        .is_err());
        // Sender bindings carry sender material only — never
        // identity, routing or receipt fields.
        assert!(serde_json::from_str::<BindingSave>(
            r#"{"binding_id":"bind-1","sender_name":"News","sender_address":"news@example.com","unsubscribe_base":"https://example.com/unsub"}"#
        )
        .is_ok());
        for body in [
            r#"{"binding_id":"bind-1","sender_name":"News","sender_address":"news@example.com","unsubscribe_base":"https://example.com/unsub","actor":"op"}"#,
            r#"{"binding_id":"bind-1","sender_name":"News","sender_address":"news@example.com","unsubscribe_base":"https://example.com/unsub","turn_id":"t"}"#,
            r#"{"binding_id":"bind-1","sender_name":"News","sender_address":"news@example.com"}"#,
            r#"{"binding_id":"preview","sender_name":"News","sender_address":"news@example.com","unsubscribe_base":"https://example.com/unsub"}"#,
        ] {
            // The reserved preview ID passes the transport grammar
            // (the daemon owns the reservation); every other body
            // above refuses here.
            if body.contains("\"binding_id\":\"preview\"") {
                assert!(serde_json::from_str::<BindingSave>(body).is_ok());
            } else {
                assert!(
                    serde_json::from_str::<BindingSave>(body).is_err(),
                    "binding save admitted {body}"
                );
            }
        }
    }
}
