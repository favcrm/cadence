//! Trusted provider metadata for discovery; it grants no execution rights.
use crate::{
    contract_fixture::ToolTable,
    error::{Error, Result},
};
use serde::Serialize;

/// Shared schemas understood by the text publication v1 broker.
pub const TEXT_PUBLICATION_INPUT_V1: &str = "text.publish.input@1";
pub const TEXT_PUBLICATION_RECEIPT_V1: &str = "text.publish.receipt@1";

/// Connection custody admits the one reviewed provider whose registered name
/// predates the hyphen-only platform identifier grammar. This is an exact
/// allowlist, not permission for caller-chosen underscore names.
pub fn provider_identifier(value: &str, what: &str) -> Result<String> {
    if value == crate::platform::agenticos_external::PLATFORM {
        Ok(value.to_owned())
    } else {
        crate::proto::identifier(value, what)
    }
}

/// AgenticOS workspace records are minted as `ws_<UUID>`. The generic
/// issuer ID schema is broader, but this connection account is a workspace,
/// not an arbitrary hosted actor ID.
pub fn provider_account_identifier(provider: &str, value: &str) -> Result<String> {
    if provider != crate::platform::agenticos_external::PLATFORM {
        return crate::proto::identifier(value, "Account");
    }
    let id = value.strip_prefix("ws_").and_then(|suffix| {
        uuid::Uuid::parse_str(suffix)
            .ok()
            .filter(|id| id.to_string() == suffix)
    });
    if id.is_none() {
        return Err(Error::rejected(
            "AgenticOS external account must be a canonical workspace ID",
        ));
    }
    Ok(value.to_owned())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySemantics {
    LocalMarkdownSink,
    UpstreamApprovalHandoff,
    MetadataRead,
    PreviewOnly,
    /// An authenticated encrypted mail-submission send (CAD-785).
    /// Serializes as `email_send`; the `validate` grammar below
    /// admits it exactly like the other reviewed semantics.
    EmailSend,
}
#[derive(Clone, Debug, Serialize)]
pub struct CapabilityDescriptor {
    pub id: String,
    pub version: u32,
    pub tools: Vec<String>,
    pub scopes: Vec<String>,
    pub effect: String,
    pub semantics: CapabilitySemantics,
}

/// A reviewed internal app-artifact action, never a legacy worker tool grant.
#[derive(Clone, Debug, Serialize)]
pub struct BoundActionMapping {
    pub capability: String,
    pub version: u32,
    pub action: String,
    pub resource_kind: String,
    pub tool: String,
    pub scopes: Vec<String>,
    pub effect: String,
    pub semantics: CapabilitySemantics,
    pub input_contract: String,
    pub output_contract: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct ProviderDescriptor {
    pub schema: u32,
    pub provider: String,
    pub revision: String,
    pub enrollment_shapes: Vec<String>,
    pub builtin_accounts: Vec<String>,
    pub capabilities: Vec<CapabilityDescriptor>,
    #[serde(default)]
    pub action_mappings: Vec<BoundActionMapping>,
}
impl ProviderDescriptor {
    pub fn validate(&self, table: &ToolTable) -> Result<()> {
        if table.manifest_version.as_deref().is_none_or(str::is_empty)
            || self.schema != 1
            || self.provider != table.platform
            || self.revision.is_empty()
            || self.capabilities.is_empty()
            // Enrollment shapes are an exact allowlist, not a prefix
            // or pattern: `token` (operator-minted scoped tokens) and
            // `smtp` (CAD-785 typed host/port/TLS/sender plus secret
            // custody). Anything else refuses.
            || self
                .enrollment_shapes
                .iter()
                .any(|s| s != "token" && s != "smtp")
        {
            return Err(Error::rejected(
                "provider connection descriptor is unavailable",
            ));
        }
        let mut ids = std::collections::HashSet::new();
        for cap in &self.capabilities {
            if cap.id.is_empty()
                || cap.version == 0
                || cap.tools.is_empty()
                || !ids.insert(&cap.id)
                || !matches!(cap.effect.as_str(), "read" | "draft" | "send")
            {
                return Err(Error::rejected("provider capability descriptor is invalid"));
            }
            for tool in &cap.tools {
                let declaration =
                    table
                        .tools
                        .iter()
                        .find(|d| d.tool == *tool)
                        .ok_or_else(|| {
                            Error::rejected("provider capability names an unreviewed tool")
                        })?;
                if declaration.effect.as_deref() != Some(cap.effect.as_str())
                    || declaration.scopes != cap.scopes
                {
                    return Err(Error::rejected(
                        "provider capability differs from reviewed tool scopes or effect",
                    ));
                }
            }
        }
        let mut actions = std::collections::HashSet::new();
        for mapping in &self.action_mappings {
            if mapping.capability == "text.publish"
                && mapping.version == 1
                && (mapping.input_contract != TEXT_PUBLICATION_INPUT_V1
                    || mapping.output_contract != TEXT_PUBLICATION_RECEIPT_V1)
            {
                return Err(Error::rejected(
                    "provider text publication contracts are incompatible",
                ));
            }
            if mapping.capability.is_empty()
                || mapping.version == 0
                || mapping.action.is_empty()
                || mapping.resource_kind != "connection_account"
                || mapping.input_contract.is_empty()
                || mapping.output_contract.is_empty()
                || !actions.insert((
                    &mapping.capability,
                    mapping.version,
                    &mapping.action,
                    &mapping.resource_kind,
                ))
            {
                return Err(Error::rejected(
                    "provider app action mapping is invalid or ambiguous",
                ));
            }
            let declaration = table
                .tools
                .iter()
                .find(|declaration| declaration.tool == mapping.tool)
                .ok_or_else(|| Error::rejected("provider app action names an unreviewed tool"))?;
            if declaration.effect.as_deref() != Some(mapping.effect.as_str())
                || declaration.scopes != mapping.scopes
                || !self.capabilities.iter().any(|cap| {
                    cap.id == mapping.capability
                        && cap.version == mapping.version
                        && cap.tools.contains(&mapping.tool)
                        && cap.scopes == mapping.scopes
                        && cap.effect == mapping.effect
                        && cap.semantics == mapping.semantics
                })
            {
                return Err(Error::rejected(
                    "provider app action differs from its reviewed capability",
                ));
            }
        }
        Ok(())
    }

    pub fn resolve_action(
        &self,
        capability: &str,
        version: u32,
        action: &str,
        resource_kind: &str,
    ) -> Result<&BoundActionMapping> {
        let mut matches = self.action_mappings.iter().filter(|mapping| {
            mapping.capability == capability
                && mapping.version == version
                && mapping.action == action
                && mapping.resource_kind == resource_kind
        });
        let mapping = matches
            .next()
            .ok_or_else(|| Error::rejected("provider does not support the declared app action"))?;
        if matches.next().is_some() {
            return Err(Error::rejected("provider app action mapping is ambiguous"));
        }
        Ok(mapping)
    }
}

pub fn registration_digest(value: &str) -> String {
    registration_digest_bytes(value.as_bytes())
}

pub fn registration_digest_bytes(value: &[u8]) -> String {
    use sha2::Digest;
    format!("sha256:{:x}", sha2::Sha256::digest(value))
}
#[cfg(test)]
mod tests {
    #[test]
    fn capability_metadata_must_match_reviewed_tools_exactly() {
        use crate::platform::PlatformAdapter;
        let adapter = crate::contract_fixture::FakePlatform::standard();
        let descriptor = adapter.connection_descriptor().unwrap();
        descriptor.validate(adapter.table()).unwrap();
        for field in ["tool", "scope", "effect"] {
            let mut bad = descriptor.clone();
            match field {
                "tool" => bad.capabilities[0].tools = vec!["invented".into()],
                "scope" => bad.capabilities[0].scopes = vec!["widgets:publish".into()],
                _ => bad.capabilities[0].effect = "send".into(),
            }
            assert!(bad.validate(adapter.table()).is_err(), "{field}");
        }
    }
    #[test]
    fn local_registration_changes_with_actual_outbox_destination() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = crate::daemon::ServeOptions::default();
        let mut second = crate::daemon::ServeOptions::default();
        crate::platform::local::register_at(
            dir.path(),
            &mut first,
            dir.path().join("one"),
            "http://127.0.0.1:3188".into(),
        );
        crate::platform::local::register_at(
            dir.path(),
            &mut second,
            dir.path().join("two"),
            "http://127.0.0.1:3188".into(),
        );
        assert_ne!(
            first.platforms["local"].connection_registration(),
            second.platforms["local"].connection_registration()
        );
        assert_eq!(
            first.platforms["local"]
                .connection_descriptor()
                .unwrap()
                .builtin_accounts,
            second.platforms["local"]
                .connection_descriptor()
                .unwrap()
                .builtin_accounts
        );
    }
}
