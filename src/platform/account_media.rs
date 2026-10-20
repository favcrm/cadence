//! Image-only normal-account transport. No enrollment, copied credential,
//! deployment pin, ambient destination or generic HTTP/CLI forwarding.
use super::agenticos_external::AgenticosExternalAdapter;
use super::connections::{
    BoundActionMapping, CapabilityDescriptor, CapabilitySemantics, ProviderDescriptor,
};
use super::{
    AppCapabilityError, AppCapabilityOutput, AppCapabilityQuote, ImageFailure, ImageReason,
    PlatformAdapter,
};
use crate::contract_fixture::{ToolTable, Verified};
use crate::error::{Error, Result};
use crate::remote_cli::{self, AccountImageIdentity};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const PLATFORM: &str = "account-images";
pub const MANIFEST: &str = "cadence-account-images@1";
const TABLE: &str = r#"{"platform":"account-images","manifest_version":"cadence-account-images@1","tools":[{"tool":"generate_image","effect":"draft","scopes":["cli.write"],"label":"Generate an image using the selected signed-in account"}]}"#;

/// Durable account identity, independent of the current login session.
/// Deserialization is not authority; resolve it against current custody.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountSource {
    pub org: String,
    pub organization_id: String,
    pub audience: String,
    pub issuer: String,
    pub subject_id: String,
}

impl From<&AccountImageIdentity> for AccountSource {
    fn from(identity: &AccountImageIdentity) -> Self {
        Self {
            org: identity.org.clone(),
            organization_id: identity.organization_id.clone(),
            audience: identity.audience.clone(),
            issuer: identity.issuer.clone(),
            subject_id: identity.subject_id.clone(),
        }
    }
}

impl AccountSource {
    pub fn handle(&self) -> String {
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(self).expect("account source serialization"))
        )
    }
}

/// A non-secret account handle, not proof of actor ownership or available funds.
pub fn account_handle(identity: &AccountImageIdentity) -> String {
    AccountSource::from(identity).handle()
}

/// Full immutable execution lineage; the broker independently compares it
/// under its private lock before renewal or any account request.
pub(crate) fn source_digest(identity: &AccountImageIdentity) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(identity).expect("account identity serialization"))
    )
}

pub(crate) fn validate_quote_source(
    identity: &AccountImageIdentity,
    revision: &str,
) -> std::result::Result<(), String> {
    if revision.len() != 128
        || !revision.bytes().all(|b| b.is_ascii_hexdigit())
        || !revision.starts_with(&source_digest(identity))
    {
        return Err(
            "saved account SESSION QUOTE is stale; request a fresh quote and approve it again"
                .into(),
        );
    }
    Ok(())
}

pub(crate) fn current_identity(
    config: &Value,
) -> std::result::Result<AccountImageIdentity, String> {
    let account = config["account"]
        .as_str()
        .ok_or("account image source is absent")?;
    let identity = identities()
        .map_err(|_| "saved account image inventory unavailable")?
        .into_iter()
        .find(|identity| account_handle(identity) == account)
        .ok_or(
            "saved account source is missing, expired or replaced; frozen image authority is stale",
        )?;
    if config["account_source"]
        != serde_json::to_value(AccountSource::from(&identity))
            .map_err(|_| "account image identity cannot be encoded")?
    {
        return Err("native account image binding lacks its exact stable source identity".into());
    }
    Ok(identity)
}

/// Called after unlocked quote I/O and reacquiring native receipt guards.
pub(crate) fn recheck_quote(config: &Value, quote: &AppCapabilityQuote) -> Result<()> {
    if config["provider"] == PLATFORM {
        let identity = current_identity(config).map_err(Error::rejected)?;
        validate_quote_source(&identity, &quote.price_revision).map_err(Error::rejected)?;
    }
    Ok(())
}

/// Offline, fresh discovery. Never resolve the registry default or renew here.
/// Invalid/expired logins are not eligible; malformed registry is an error,
/// rather than permission to fall back to another source of destinations.
pub fn identities() -> Result<Vec<AccountImageIdentity>> {
    let mut identities = Vec::new();
    for target in remote_cli::account_image_targets()? {
        if let Ok(identity) = remote_cli::account_image_identity(&target) {
            identities.push(identity);
        }
    }
    identities.sort_by_key(account_handle);
    Ok(identities)
}

pub struct AccountMediaAdapter {
    table: ToolTable,
}

impl AccountMediaAdapter {
    pub fn new() -> Result<Self> {
        Ok(Self {
            table: ToolTable::from_json(
                &serde_json::from_str(TABLE).expect("reviewed image table"),
            )?,
        })
    }

    fn bound_adapter(
        &self,
        credential: &[u8],
        config: &Value,
    ) -> std::result::Result<AgenticosExternalAdapter, String> {
        let mapping = &config["mapping"];
        if !credential.is_empty()
            || config["provider"] != PLATFORM
            || config["connection_kind"] != "builtin"
            || config["connection_id"].as_str().is_none_or(str::is_empty)
            || mapping["capability"] != "media.generate"
            || mapping["version"] != 1
            || mapping["action"] != "generate_image"
            || mapping["resource_kind"] != "connection_account"
            || mapping["effect"] != "draft"
            || mapping["tool"] != "generate_image"
        {
            return Err("frozen account image binding is invalid".into());
        }
        let identity = current_identity(config)?;
        AgenticosExternalAdapter::account_image(identity)
            .map_err(|_| "account image adapter unavailable".into())
    }
}

pub fn attach(opts: &mut crate::daemon::ServeOptions) -> Result<()> {
    let adapter = AccountMediaAdapter::new()?;
    if let Some(existing) = opts.platforms.get(PLATFORM) {
        if existing.connection_registration() != adapter.connection_registration() {
            return Err(Error::rejected(
                "account image transport conflicts with registered provider",
            ));
        }
    } else {
        opts.platforms
            .insert(PLATFORM.into(), std::sync::Arc::new(adapter));
    }
    Ok(())
}

impl PlatformAdapter for AccountMediaAdapter {
    fn table(&self) -> &ToolTable {
        &self.table
    }

    /// `execute` refuses: this adapter only runs under a frozen app-run
    /// image capability, never a plain `platform_call`.
    fn direct_call(&self, _tool: &str) -> bool {
        false
    }

    fn connection_descriptor(&self) -> Option<ProviderDescriptor> {
        Some(ProviderDescriptor {
            schema: 1,
            provider: PLATFORM.into(),
            revision: "account-image-connections/1".into(),
            enrollment_shapes: Vec::new(),
            builtin_accounts: identities()
                .unwrap_or_default()
                .iter()
                .map(account_handle)
                .collect(),
            capabilities: vec![CapabilityDescriptor {
                id: "media.generate".into(),
                version: 1,
                tools: vec!["generate_image".into()],
                scopes: vec!["cli.write".into()],
                effect: "draft".into(),
                semantics: CapabilitySemantics::PreviewOnly,
            }],
            action_mappings: vec![BoundActionMapping {
                capability: "media.generate".into(),
                version: 1,
                action: "generate_image".into(),
                resource_kind: "connection_account".into(),
                tool: "generate_image".into(),
                scopes: vec!["cli.write".into()],
                effect: "draft".into(),
                semantics: CapabilitySemantics::PreviewOnly,
                input_contract: "media.image.prompt@1".into(),
                output_contract: "media.image.asset@1".into(),
            }],
        })
    }

    fn connection_registration(&self) -> Option<String> {
        Some(super::connections::registration_digest(
            "account-images:normal-cli-custody:fixed-board-image@1",
        ))
    }

    fn reported_manifest_version(&self) -> Option<String> {
        Some(MANIFEST.into())
    }

    fn app_credentialless_account(&self, account: &str) -> bool {
        identities().is_ok_and(|rows| {
            rows.iter()
                .any(|identity| account_handle(identity) == account)
        })
    }

    fn quote_app_capability(
        &self,
        credential: &[u8],
        binding: &Value,
    ) -> std::result::Result<AppCapabilityQuote, String> {
        self.bound_adapter(credential, &binding["config"])?
            .quote_image(credential, binding)
    }

    fn execute_app_capability(
        &self,
        credential: &[u8],
        authority: &Value,
        input: &Value,
        key: &str,
    ) -> std::result::Result<AppCapabilityOutput, String> {
        self.execute_app_capability_outcome(credential, authority, input, key)
            .map_err(|e| e.to_string())
    }

    fn execute_app_capability_outcome(
        &self,
        credential: &[u8],
        authority: &Value,
        input: &Value,
        key: &str,
    ) -> std::result::Result<AppCapabilityOutput, AppCapabilityError> {
        self.execute_app_image(credential, authority, input, key)
            .map_err(Into::into)
    }

    fn execute_app_image(
        &self,
        credential: &[u8],
        authority: &Value,
        input: &Value,
        key: &str,
    ) -> std::result::Result<AppCapabilityOutput, ImageFailure> {
        self.bound_adapter(credential, &authority["binding"]["config"])
            .map_err(|detail| ImageFailure::refused(ImageReason::NotAuthorized, detail))?
            .call_image_coded(credential, authority, input, key)
    }

    fn read_app_media_original(
        &self,
        credential: &[u8],
        config: &Value,
        descriptor: &Value,
    ) -> std::result::Result<(String, Vec<u8>), String> {
        self.bound_adapter(credential, config)?
            .read_app_media_original(credential, config, descriptor)
    }

    fn preview(&self, account: &str, tool: &str, _input: &Value) -> String {
        format!("Saved account image {tool} for bound source {account}")
    }
    fn execute(
        &self,
        _credential: &[u8],
        _tool: &str,
        _input: &Value,
        _key: &str,
        _expected_hash: Option<&str>,
    ) -> std::result::Result<Value, String> {
        Err("account images require a native frozen app image capability".into())
    }
    fn read_back(&self, _tool: &str, _input: &Value) -> Verified {
        Verified::Unknown
    }
    fn source_hash(&self, _agent: &str, _source: &str) -> Option<String> {
        None
    }
}

#[cfg(test)]
#[path = "account_media/cad1459_login_lineage_acceptance.rs"]
mod cad1459_login_lineage_acceptance;
