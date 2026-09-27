//! Trusted provider metadata for discovery; it grants no execution rights.
use crate::{
    contract_fixture::ToolTable,
    error::{Error, Result},
};
use serde::Serialize;
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySemantics {
    LocalMarkdownSink,
    UpstreamApprovalHandoff,
    MetadataRead,
    PreviewOnly,
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
            || self.enrollment_shapes.iter().any(|s| s != "token")
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
