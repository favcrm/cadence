//! Host-owned registry for the first generic app assistant action set.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::BTreeSet;

#[cfg(test)]
use serde_json::json;
use serde_json::Value;

mod validation;

use validation::schema;
pub use validation::{schema_accepts, validate_input};

#[derive(Clone)]
pub struct RegisteredAction {
    pub id: &'static str,
    pub effect: &'static str,
    pub confirmation: &'static str,
    pub handler_version: &'static str,
    pub input_schema: Value,
}

pub fn registered_action(id: &str) -> Option<RegisteredAction> {
    let (effect, confirmation) = match id {
        "customers.search" | "customers.show" | "segments.list" | "segments.show"
        | "segments.preview" | "campaigns.list" | "campaigns.show" => ("read", "none"),
        "customers.notes.list" => ("read", "none"),
        "customers.notes.create" => ("write", "none"),
        "customers.notes.delete" => ("delete", "none"),
        "segments.save" | "segments.save_static" => ("write", "none"),
        "segments.delete" => ("delete", "none"),
        "campaigns.create_draft" => ("draft", "none"),
        "campaigns.apply" | "campaigns.discard" => ("write", "none"),
        "campaigns.approve" => ("write", "none"),
        "audiences.prepare" => ("write", "none"),
        "audiences.show" => ("read", "none"),
        "sends.list" | "sends.show" => ("read", "none"),
        "email.draft" => ("proposal", "none"),
        "customer.tags.update" => ("write", "permission_required"),
        "social.posts.fetch" => ("read", "permission_required"),
        "social.draft.create" => ("draft", "permission_required"),
        "social.draft.compose" => ("draft", "none"),
        "social.drafts.attach_image" => ("draft", "none"),
        "social.drafts.status" => ("read", "none"),
        "media.refine.prepare" => ("draft", "permission_required"),
        "whatsapp.conversation.context" => ("read", "none"),
        "whatsapp.reply.draft" => ("proposal", "none"),
        _ => return None,
    };
    Some(RegisteredAction {
        id: match id {
            "customers.search" => "customers.search",
            "customers.show" => "customers.show",
            "segments.list" => "segments.list",
            "segments.show" => "segments.show",
            "segments.preview" => "segments.preview",
            "customers.notes.list" => "customers.notes.list",
            "customers.notes.create" => "customers.notes.create",
            "customers.notes.delete" => "customers.notes.delete",
            "segments.save" => "segments.save",
            "segments.save_static" => "segments.save_static",
            "segments.delete" => "segments.delete",
            "campaigns.list" => "campaigns.list",
            "campaigns.show" => "campaigns.show",
            "campaigns.create_draft" => "campaigns.create_draft",
            "campaigns.apply" => "campaigns.apply",
            "campaigns.discard" => "campaigns.discard",
            "campaigns.approve" => "campaigns.approve",
            "audiences.prepare" => "audiences.prepare",
            "audiences.show" => "audiences.show",
            "sends.list" => "sends.list",
            "sends.show" => "sends.show",
            "email.draft" => "email.draft",
            "customer.tags.update" => "customer.tags.update",
            "social.posts.fetch" => "social.posts.fetch",
            "social.draft.create" => "social.draft.create",
            "social.draft.compose" => "social.draft.compose",
            "social.drafts.attach_image" => "social.drafts.attach_image",
            "social.drafts.status" => "social.drafts.status",
            "media.refine.prepare" => "media.refine.prepare",
            "whatsapp.conversation.context" => "whatsapp.conversation.context",
            "whatsapp.reply.draft" => "whatsapp.reply.draft",
            _ => unreachable!(),
        },
        effect,
        confirmation,
        handler_version: "v1",
        input_schema: schema(id)?,
    })
}

pub fn registered_ids() -> &'static [&'static str] {
    &[
        "customers.search",
        "customers.show",
        "segments.list",
        "segments.show",
        "segments.preview",
        "customers.notes.list",
        "customers.notes.create",
        "customers.notes.delete",
        "segments.save",
        "segments.save_static",
        "segments.delete",
        "campaigns.list",
        "campaigns.show",
        "campaigns.create_draft",
        "campaigns.apply",
        "campaigns.discard",
        "campaigns.approve",
        "audiences.prepare",
        "audiences.show",
        "sends.list",
        "sends.show",
        "email.draft",
        "customer.tags.update",
        "social.posts.fetch",
        "social.draft.create",
        "social.draft.compose",
        "social.drafts.attach_image",
        "social.drafts.status",
        "media.refine.prepare",
        "whatsapp.conversation.context",
        "whatsapp.reply.draft",
    ]
}

/// CAD-1518: what one installation's consented bundle declares. This is the
/// only allow-list for assistant actions and scoped chat verbs: an install
/// reaches exactly what it declared, and nothing is decided by app name.
/// `app` and `title` come from the installed manifest and only label the
/// chat turn.
#[derive(Clone, Debug, Default)]
pub struct Declared {
    pub app: String,
    pub title: String,
    pub actions: BTreeSet<String>,
    pub attachments: BTreeSet<String>,
}

/// What a scoped-chat RPC requires the installation to have declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopedNeed {
    /// At least one registered action in `app-assistant.json`.
    AnyAction,
    /// This registered action in `app-assistant.json`.
    Action(&'static str),
    /// This host attachment in `app-chat.json`.
    Attachment(&'static str),
}

impl Declared {
    pub fn admits(&self, need: ScopedNeed) -> bool {
        match need {
            ScopedNeed::AnyAction => !self.actions.is_empty(),
            ScopedNeed::Action(id) => self.actions.contains(id),
            ScopedNeed::Attachment(id) => self.attachments.contains(id),
        }
    }
}

/// The declaration each scoped-chat RPC needs. A method with no row is not
/// a scoped verb and is refused.
pub fn scoped_need(method: &str) -> Option<ScopedNeed> {
    use ScopedNeed::{Action, AnyAction, Attachment};
    Some(match method {
        "app_assistant_actions" | "app_assistant_invoke" | "app_assistant_operation_show" => {
            AnyAction
        }
        "app_segment_assistant_list" => Action("segments.list"),
        "app_segment_assistant_show" => Action("segments.show"),
        "app_segment_assistant_preview" => Action("segments.preview"),
        "app_segment_assistant_save" => Action("segments.save"),
        "app_content_assistant_draft"
        | "app_content_assistant_propose"
        | "app_content_assistant_proposals"
        | "app_content_assistant_proposal_show" => Action("email.draft"),
        "app_record_csv_assistant_preview" | "app_record_csv_assistant_import" => {
            Attachment("csv-import")
        }
        _ => return None,
    })
}

#[cfg(test)]
mod soc32_tests {
    use super::*;

    #[test]
    fn soc32_actions_are_registered_with_closed_schemas() {
        for id in ["social.draft.compose", "social.drafts.attach_image"] {
            let action = registered_action(id).unwrap();
            assert_eq!(action.effect, "draft");
            assert_eq!(action.confirmation, "none");
            assert!(registered_ids().contains(&id));
            assert!(schema_accepts(id, &action.input_schema));
        }
        validate_input(
            &registered_action("social.draft.compose")
                .unwrap()
                .input_schema,
            &json!({"caption": "A caption", "brief": "a brief"}),
        )
        .unwrap();
        assert!(validate_input(
            &registered_action("social.draft.compose")
                .unwrap()
                .input_schema,
            &json!({"caption": ""}),
        )
        .is_err());
        validate_input(
            &registered_action("social.drafts.attach_image")
                .unwrap()
                .input_schema,
            &json!({"draft_id": "soc-1", "expected_revision": 2, "asset_id": "up-1"}),
        )
        .unwrap();
        assert!(validate_input(
            &registered_action("social.drafts.attach_image")
                .unwrap()
                .input_schema,
            &json!({"draft_id": "soc-1", "expected_revision": 0, "asset_id": "up-1"}),
        )
        .is_err());
    }
}
