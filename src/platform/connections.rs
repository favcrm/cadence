//! Trusted provider metadata for discovery; it grants no execution rights.
use crate::{
    contract_fixture::ToolTable,
    error::{Error, Result},
};
use serde::Serialize;
#[derive(Clone, Debug, Serialize)]
pub struct CapabilityDescriptor {
    pub id: String,
    pub version: u32,
    pub tools: Vec<String>,
    pub scopes: Vec<String>,
    pub effect: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct ProviderDescriptor {
    pub schema: u32,
    pub provider: String,
    pub revision: String,
    pub enrollment_shapes: Vec<String>,
    pub builtin_accounts: Vec<String>,
    pub capabilities: Vec<CapabilityDescriptor>,
}
impl ProviderDescriptor {
    pub fn validate(&self, table: &ToolTable) -> Result<()> {
        if self.schema != 1
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
        Ok(())
    }
}

pub fn registration_digest(value: &str) -> String {
    use sha2::Digest;
    format!("sha256:{:x}", sha2::Sha256::digest(value.as_bytes()))
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
