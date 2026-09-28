//! The AgenticOS external provider door, separate from its hosted publisher.
//! Only app-run capability authority may execute these fixed, reviewed tools.
//! A token stays in custody; the upstream door derives its company from it.
mod source;

use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::contract_fixture::{ToolTable, Verified};
use crate::error::{Error, Result};
use crate::platform::adapter::PlatformAdapter;
use crate::platform::connections::{
    BoundActionMapping, CapabilityDescriptor, CapabilitySemantics, ProviderDescriptor,
};
use crate::platform::{AppCapabilityOutput, AppCapabilityQuote};

pub const PLATFORM: &str = "agenticos_external";
pub const MANIFEST_PIN: &str = "agenticos-external-provider-tools@1";
const POSTS_TOOL: &str = "scrapecreators.instagram.user.posts";
const IMAGE_TOOL: &str = "minimax.image-gen.from_text";
const CALL_PATH: &str = "/v1/runtime/tools/call";
const RESPONSE_CAP: u64 = 1024 * 1024;

const TABLE_JSON: &str = r#"{
    "platform":"agenticos_external",
    "manifest_version":"agenticos-external-provider-tools@1",
    "tools":[
        {"tool":"scrapecreators.instagram.user.posts","effect":"read","scopes":["provider.read"],"label":"Read public Instagram profile posts"},
        {"tool":"minimax.image-gen.from_text","effect":"draft","scopes":["provider.draft"],"label":"Generate an image draft"}
    ]
}"#;

pub struct AgenticosExternalAdapter {
    table: ToolTable,
    base: String,
    deployment_pin: Option<String>,
    http: ureq::Agent,
}

impl AgenticosExternalAdapter {
    /// The pin is an image-owner assertion about this exact deployed origin,
    /// never a claim made by an app, connection credential or HTTP response.
    pub fn with_deployment_pin(base: &str, deployment_pin: Option<&str>) -> Result<Self> {
        let base = valid_base(base)?;
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .http_status_as_error(false)
            .max_redirects(0)
            .build();
        let table = ToolTable::from_json(&serde_json::from_str(TABLE_JSON).expect("table JSON"))
            .expect("reviewed tool table");
        let adapter = Self {
            table,
            base,
            deployment_pin: deployment_pin.map(str::to_owned),
            http: ureq::Agent::new_with_config(config),
        };
        adapter
            .connection_descriptor()
            .expect("descriptor")
            .validate(&adapter.table)?;
        Ok(adapter)
    }

    fn quote_fixed(
        &self,
        credential: &[u8],
        binding: &Value,
    ) -> std::result::Result<(u64, String), String> {
        let config = &binding["config"];
        let mapping = &config["mapping"];
        if config["provider"] != PLATFORM
            || config["account"].as_str().is_none_or(str::is_empty)
            || config["connection_id"].as_str().is_none_or(str::is_empty)
            || mapping["resource_kind"] != "connection_account"
        {
            return Err("frozen provider binding is invalid".into());
        }
        let tool = match (
            mapping["capability"].as_str(),
            mapping["version"].as_u64(),
            mapping["action"].as_str(),
            mapping["effect"].as_str(),
            mapping["tool"].as_str(),
        ) {
            (Some("social.read"), Some(1), Some("list_posts"), Some("read"), Some(POSTS_TOOL)) => {
                POSTS_TOOL
            }
            (
                Some("media.generate"),
                Some(1),
                Some("generate_image"),
                Some("draft"),
                Some(IMAGE_TOOL),
            ) => IMAGE_TOOL,
            _ => return Err("provider quote names an unreviewed action".into()),
        };
        let token = std::str::from_utf8(credential)
            .map_err(|_| "AgenticOS provider credential is invalid UTF-8")?;
        if token.is_empty() || token.len() > 8192 || token.chars().any(char::is_whitespace) {
            return Err("AgenticOS provider credential has an invalid shape".into());
        }
        let url = format!("{}/v1/runtime/tools/{tool}", self.base);
        let mut response = self
            .http
            .get(&url)
            .header("authorization", &format!("Bearer {token}"))
            .call()
            .map_err(|_| "AgenticOS provider quote could not reach the provider")?;
        let status = response.status().as_u16();
        let bytes = response
            .body_mut()
            .with_config()
            .limit(64 * 1024)
            .read_to_vec()
            .map_err(|_| "AgenticOS provider quote exceeds the supported bound")?;
        let envelope: Value =
            serde_json::from_slice(&bytes).map_err(|_| "AgenticOS provider quote is not JSON")?;
        if status != 200 || envelope["ok"] != true {
            let code = envelope["error"]["code"]
                .as_str()
                .filter(|code| {
                    code.len() <= 64
                        && code
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                })
                .unwrap_or("unavailable");
            return Err(format!("AgenticOS provider quote refused: {code}"));
        }
        quote_view(
            tool,
            mapping["effect"].as_str().unwrap_or(""),
            &envelope["data"],
        )
    }

    fn call_source(
        &self,
        credential: &[u8],
        authority: &Value,
        input: &Value,
        idempotency_key: &str,
    ) -> std::result::Result<Value, String> {
        let handle = validate_source_authority(authority, input)?;
        let token = std::str::from_utf8(credential)
            .map_err(|_| "AgenticOS provider credential is invalid UTF-8")?;
        if token.is_empty() || token.len() > 8192 || token.chars().any(char::is_whitespace) {
            return Err("AgenticOS provider credential has an invalid shape".into());
        }
        if idempotency_key.len() < 8
            || idempotency_key.len() > 200
            || !idempotency_key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err("provider idempotency key is invalid".into());
        }
        let url = format!("{}{CALL_PATH}", self.base);
        let ceiling = frozen_charge_ceiling(authority)?;
        let mut response = self
            .http
            .post(&url)
            .header("authorization", &format!("Bearer {token}"))
            .header("idempotency-key", idempotency_key)
            .send_json(json!({
                "slug": POSTS_TOOL,
                "query": {"handle": handle},
                "max_charge_minor": ceiling,
            }))
            .map_err(|_| "AgenticOS source request could not reach the provider")?;
        let status = response.status().as_u16();
        let bytes = response
            .body_mut()
            .with_config()
            .limit(RESPONSE_CAP)
            .read_to_vec()
            .map_err(|_| "AgenticOS source response exceeds the supported bound")?;
        let envelope: Value =
            serde_json::from_slice(&bytes).map_err(|_| "AgenticOS source response is not JSON")?;
        if envelope["ok"] != true || status != 200 {
            let code = envelope["error"]["code"]
                .as_str()
                .filter(|code| {
                    code.len() <= 64
                        && code
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                })
                .unwrap_or("unavailable");
            return Err(format!("AgenticOS source read refused: {code}"));
        }
        let data = envelope
            .get("data")
            .ok_or("AgenticOS source receipt is missing")?;
        if data["slug"] != POSTS_TOOL {
            return Err("AgenticOS source tool identity changed".into());
        }
        let charged = money_micros(&data["price"])
            .ok_or("AgenticOS source settled receipt has invalid charge")?;
        if charged > ceiling || !data["repeated"].is_boolean() {
            return Err(
                "AgenticOS source settled receipt exceeds the approved charge or is malformed"
                    .into(),
            );
        }
        let upstream = data
            .get("result")
            .ok_or("AgenticOS source data is missing")?;
        let mut normalized = source::normalize_posts(handle, upstream)?;
        // The quoted admin charge is provider-owned provenance, never a
        // caller-controlled cost decision or permission to make another call.
        normalized["charge"] = data["price"].clone();
        normalized["repeated"] = data["repeated"].clone();
        if serde_json::to_vec(&normalized)
            .map_err(|_| "source receipt cannot be serialized")?
            .len()
            > 256 * 1024
        {
            return Err("source receipt exceeds the durable result bound".into());
        }
        Ok(normalized)
    }
}

fn money_micros(value: &Value) -> Option<u64> {
    if value["currency"] != "USD" || value["scale"] != 6 {
        return None;
    }
    let amount = value["amount"].as_str()?;
    let (whole, fractional) = amount.split_once('.')?;
    if whole.is_empty()
        || whole.len() > 6
        || fractional.len() != 6
        || !whole
            .bytes()
            .chain(fractional.bytes())
            .all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    whole
        .parse::<u64>()
        .ok()?
        .checked_mul(1_000_000)?
        .checked_add(fractional.parse::<u64>().ok()?)
}

fn quote_view(
    tool: &str,
    effect: &str,
    view: &Value,
) -> std::result::Result<(u64, String), String> {
    // Older deployments accepted a paid POST without enforcing an approved
    // ceiling. Never quote or call them, even if trusted metadata pins their
    // tool manifest.
    if view["chargePrecondition"] != "max_charge_minor@1" {
        return Err("AgenticOS provider does not enforce the approved charge ceiling".into());
    }
    if view["slug"] != tool || view["effect"] != effect {
        return Err("AgenticOS provider quote changed tool or effect".into());
    }
    let per_call = money_micros(&view["price"])
        .ok_or("AgenticOS provider quote has invalid per-call price")?;
    let per_unit = if view["unitPrice"].is_null() {
        0
    } else {
        money_micros(&view["unitPrice"])
            .ok_or("AgenticOS provider quote has invalid per-unit price")?
    };
    let total = per_call
        .checked_add(per_unit)
        .filter(|value| (1..=1_000_000_000).contains(value))
        .ok_or("AgenticOS provider quote exceeds the supported price bound")?;
    let reviewed =
        json!({"slug":tool,"effect":effect,"price":view["price"],"unitPrice":view["unitPrice"]});
    let bytes = serde_json::to_vec(&reviewed)
        .map_err(|_| "AgenticOS provider quote cannot be serialized")?;
    let revision = format!("sha256:{:x}", Sha256::digest(&bytes));
    Ok((total, revision))
}

fn frozen_charge_ceiling(authority: &Value) -> std::result::Result<u64, String> {
    let quote = &authority["quote"];
    let amount = quote["total_price_micros"]
        .as_u64()
        .filter(|value| (1..=1_000_000_000).contains(value))
        .ok_or("run has no approved provider charge ceiling")?;
    if quote["schema"] != 1
        || quote["currency"] != "USD"
        || quote["units"] != 1
        || quote["unit_price_micros"].as_u64() != Some(amount)
        || quote["price_revision"].as_str().is_none_or(str::is_empty)
    {
        return Err("run has an invalid frozen provider quote".into());
    }
    Ok(amount)
}

fn valid_base(base: &str) -> Result<String> {
    let base = base.trim_end_matches('/');
    let uri: ureq::http::Uri = base
        .parse()
        .map_err(|_| Error::rejected("external provider origin is invalid"))?;
    if !matches!(uri.scheme_str(), Some("https") | Some("http"))
        || uri.authority().is_none()
        || uri
            .authority()
            .is_some_and(|authority| authority.as_str().contains('@'))
        || !matches!(uri.path(), "" | "/")
        || uri.query().is_some()
    {
        return Err(Error::rejected(
            "external provider origin must be an HTTP(S) authority without credentials or path",
        ));
    }
    let host = uri
        .host()
        .ok_or_else(|| Error::rejected("external provider origin has no host"))?;
    if uri.scheme_str() == Some("http")
        && host != "127.0.0.1"
        && host != "localhost"
        && host != "[::1]"
    {
        return Err(Error::rejected(
            "external provider origin must use HTTPS outside local tests",
        ));
    }
    Ok(base.to_owned())
}

fn validate_source_authority<'a>(
    authority: &'a Value,
    input: &Value,
) -> std::result::Result<&'a str, String> {
    let config = &authority["binding"]["config"];
    let mapping = &config["mapping"];
    if authority["schema"] != 1
        || authority["slot"] != "source"
        || config["provider"] != PLATFORM
        || config["install_id"] != authority["install_id"]
        || config["connection_id"].as_str().is_none()
        || config["account"].as_str().is_none()
        || mapping["capability"] != "social.read"
        || mapping["version"] != 1
        || mapping["action"] != "list_posts"
        || mapping["resource_kind"] != "connection_account"
        || mapping["tool"] != POSTS_TOOL
        || mapping["effect"] != "read"
    {
        return Err("frozen source binding does not authorize this provider action".into());
    }
    let handle = authority["inputs"]["profile_handle"]
        .as_str()
        .filter(|handle| source::valid_handle(handle))
        .ok_or("frozen source profile handle is invalid")?;
    let fields = input.as_object().ok_or("source input must be an object")?;
    if !fields.is_empty()
        && (fields.len() != 1 || fields.get("handle").and_then(Value::as_str) != Some(handle))
    {
        return Err("source input attempts to change the frozen profile handle".into());
    }
    Ok(handle)
}

pub fn register_with_deployment_pin(
    opts: &mut crate::daemon::ServeOptions,
    base: &str,
    deployment_pin: Option<&str>,
) -> Result<()> {
    opts.platforms.insert(
        PLATFORM.into(),
        std::sync::Arc::new(AgenticosExternalAdapter::with_deployment_pin(
            base,
            deployment_pin,
        )?),
    );
    Ok(())
}

/// External access is opt-in per daemon. Only trusted deployment metadata
/// can attest a current reviewed manifest; absent metadata keeps calls shut.
pub fn attach(opts: &mut crate::daemon::ServeOptions) -> Result<()> {
    if opts.platforms.contains_key(PLATFORM) {
        return Ok(());
    }
    let Ok(base) = std::env::var("CADENCE_AGENTICOS_EXTERNAL_URL") else {
        return Ok(());
    };
    let base = valid_base(&base)?;
    let metadata = match &opts.provider_deployments {
        Some(value) => Some(value.clone()),
        None => super::deployments::load()?,
    };
    let pin = metadata
        .as_ref()
        .and_then(|value| value.pin(PLATFORM, &base));
    register_with_deployment_pin(opts, &base, pin)
}

impl PlatformAdapter for AgenticosExternalAdapter {
    fn quote_app_capability(
        &self,
        credential: &[u8],
        binding: &Value,
    ) -> std::result::Result<AppCapabilityQuote, String> {
        let (total, revision) = self.quote_fixed(credential, binding)?;
        Ok(AppCapabilityQuote {
            schema: 1,
            currency: "USD".into(),
            unit_price_micros: total,
            units: 1,
            total_price_micros: total,
            price_revision: revision,
        })
    }

    fn execute_app_capability(
        &self,
        credential: &[u8],
        authority: &Value,
        input: &Value,
        idempotency_key: &str,
    ) -> std::result::Result<AppCapabilityOutput, String> {
        match authority["slot"].as_str() {
            Some("source") => Ok(AppCapabilityOutput {
                result: self.call_source(credential, authority, input, idempotency_key)?,
                asset: None,
            }),
            // An image result must be downloaded, bounded and stored as
            // durable bytes before any draft can cite it. No image workflow
            // or external generation dispatch is exposed in this slice.
            _ => Err("AgenticOS capability slot has no reviewed execution adapter".into()),
        }
    }

    fn table(&self) -> &ToolTable {
        &self.table
    }

    fn connection_descriptor(&self) -> Option<ProviderDescriptor> {
        Some(ProviderDescriptor {
            schema: 1,
            provider: PLATFORM.into(),
            revision: "agenticos-external-connections/1".into(),
            enrollment_shapes: vec!["token".into()],
            builtin_accounts: vec![],
            capabilities: vec![
                CapabilityDescriptor {
                    id: "social.read".into(),
                    version: 1,
                    tools: vec![POSTS_TOOL.into()],
                    scopes: vec!["provider.read".into()],
                    effect: "read".into(),
                    semantics: CapabilitySemantics::MetadataRead,
                },
                CapabilityDescriptor {
                    id: "media.generate".into(),
                    version: 1,
                    tools: vec![IMAGE_TOOL.into()],
                    scopes: vec!["provider.draft".into()],
                    effect: "draft".into(),
                    semantics: CapabilitySemantics::PreviewOnly,
                },
            ],
            action_mappings: vec![
                BoundActionMapping {
                    capability: "social.read".into(),
                    version: 1,
                    action: "list_posts".into(),
                    resource_kind: "connection_account".into(),
                    tool: POSTS_TOOL.into(),
                    scopes: vec!["provider.read".into()],
                    effect: "read".into(),
                    semantics: CapabilitySemantics::MetadataRead,
                    input_contract: "social.posts.query@1".into(),
                    output_contract: "social.posts.receipt@1".into(),
                },
                BoundActionMapping {
                    capability: "media.generate".into(),
                    version: 1,
                    action: "generate_image".into(),
                    resource_kind: "connection_account".into(),
                    tool: IMAGE_TOOL.into(),
                    scopes: vec!["provider.draft".into()],
                    effect: "draft".into(),
                    semantics: CapabilitySemantics::PreviewOnly,
                    input_contract: "media.image.prompt@1".into(),
                    output_contract: "media.image.asset@1".into(),
                },
            ],
        })
    }

    fn connection_registration(&self) -> Option<String> {
        Some(super::connections::registration_digest(&format!(
            "{PLATFORM}:{}:{:?}",
            self.base, self.deployment_pin
        )))
    }

    fn reported_manifest_version(&self) -> Option<String> {
        self.deployment_pin.clone()
    }

    fn preview(&self, account: &str, tool: &str, _input: &Value) -> String {
        format!("AgenticOS external provider {tool} for bound account {account}")
    }

    fn execute(
        &self,
        _credential: &[u8],
        _tool: &str,
        _input: &Value,
        _idempotency_key: &str,
        _expected_hash: Option<&str>,
    ) -> std::result::Result<Value, String> {
        Err("AgenticOS external tools require a scoped app-run capability".into())
    }

    fn read_back(&self, _tool: &str, _input: &Value) -> Verified {
        Verified::Unknown
    }
    fn source_hash(&self, _agent: &str, _source: &str) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authority() -> Value {
        json!({"schema":1,"install_id":"install1","slot":"source","inputs":{"profile_handle":"juicysuite_crm"},"binding":{"config":{
            "provider":PLATFORM,"account":"company1","install_id":"install1","connection_id":"conn1","mapping":{
                "capability":"social.read","version":1,"action":"list_posts","resource_kind":"connection_account","tool":POSTS_TOOL,"effect":"read"}}}})
    }

    #[test]
    fn source_call_rejects_worker_selected_account_tool_and_handle() {
        let mut proof = authority();
        assert_eq!(
            validate_source_authority(&proof, &json!({})).unwrap(),
            "juicysuite_crm"
        );
        assert!(validate_source_authority(&proof, &json!({"handle":"other"})).is_err());
        assert!(validate_source_authority(&proof, &json!({"tool":IMAGE_TOOL})).is_err());
        proof["binding"]["config"]["mapping"]["tool"] = json!(IMAGE_TOOL);
        assert!(validate_source_authority(&proof, &json!({})).is_err());
        proof["binding"]["config"]["mapping"]["tool"] = json!(POSTS_TOOL);
        proof["binding"]["config"]["provider"] = json!("agenticos");
        assert!(validate_source_authority(&proof, &json!({})).is_err());
    }

    #[test]
    fn external_adapter_never_exposes_legacy_tool_execution() {
        let adapter =
            AgenticosExternalAdapter::with_deployment_pin("https://api.example.test", None)
                .unwrap();
        assert!(adapter
            .execute(b"token", POSTS_TOOL, &json!({}), "key", None)
            .is_err());
        assert!(adapter.reported_manifest_version().is_none());
        adapter
            .connection_descriptor()
            .unwrap()
            .validate(adapter.table())
            .unwrap();
        assert!(
            AgenticosExternalAdapter::with_deployment_pin("http://api.example.test", None).is_err()
        );
    }

    #[test]
    fn quote_uses_admin_price_for_one_fixed_unit_and_detects_changes() {
        let view = json!({"slug":POSTS_TOOL,"effect":"read","chargePrecondition":"max_charge_minor@1","price":{"currency":"USD","scale":6,"amount":"0.001880"},"unitPrice":{"currency":"USD","scale":6,"amount":"0.000120"}});
        let (amount, revision) = quote_view(POSTS_TOOL, "read", &view).unwrap();
        assert_eq!(amount, 2_000);
        assert!(revision.starts_with("sha256:"));
        let mut changed = view.clone();
        changed["unitPrice"]["amount"] = json!("0.000121");
        assert_ne!(
            quote_view(POSTS_TOOL, "read", &changed).unwrap().1,
            revision
        );
        changed["unitPrice"]["amount"] = json!("0.000120");
        changed["effect"] = json!("send");
        assert!(quote_view(POSTS_TOOL, "read", &changed).is_err());
        changed["effect"] = json!("read");
        changed["slug"] = json!(IMAGE_TOOL);
        assert!(quote_view(POSTS_TOOL, "read", &changed).is_err());
    }

    #[test]
    fn malformed_or_over_ceiling_money_is_not_an_executable_quote() {
        for amount in ["-1.000000", "1.0", "1e3", "1000000.000000", "0.000000"] {
            let view = json!({"slug":POSTS_TOOL,"effect":"read","chargePrecondition":"max_charge_minor@1","price":{"currency":"USD","scale":6,"amount":amount},"unitPrice":null});
            assert!(quote_view(POSTS_TOOL, "read", &view).is_err(), "{amount}");
        }
        let view = json!({"slug":POSTS_TOOL,"effect":"read","chargePrecondition":"max_charge_minor@1","price":{"currency":"HKD","scale":6,"amount":"0.001000"},"unitPrice":null});
        assert!(quote_view(POSTS_TOOL, "read", &view).is_err());
    }

    #[test]
    fn old_provider_without_atomic_price_contract_cannot_be_quoted() {
        let mut view = json!({"slug":POSTS_TOOL,"effect":"read","price":{"currency":"USD","scale":6,"amount":"0.001000"},"unitPrice":null});
        assert!(quote_view(POSTS_TOOL, "read", &view).is_err());
        view["chargePrecondition"] = json!("unknown");
        assert!(quote_view(POSTS_TOOL, "read", &view).is_err());
        view["chargePrecondition"] = json!("max_charge_minor@1");
        assert!(quote_view(POSTS_TOOL, "read", &view).is_ok());
    }

    #[test]
    fn paid_call_ceiling_must_be_the_frozen_one_unit_quote() {
        let mut proof = authority();
        assert!(frozen_charge_ceiling(&proof).is_err());
        proof["quote"] = json!({"schema":1,"currency":"USD","unit_price_micros":2000,"units":1,"total_price_micros":2000,"price_revision":"sha256:price"});
        assert_eq!(frozen_charge_ceiling(&proof).unwrap(), 2000);
        proof["quote"]["total_price_micros"] = json!(3000);
        assert!(frozen_charge_ceiling(&proof).is_err());
        proof["quote"]["total_price_micros"] = json!(2000);
        proof["quote"]["units"] = json!(2);
        assert!(frozen_charge_ceiling(&proof).is_err());
    }

    #[test]
    fn provider_http_call_uses_fixed_slug_frozen_handle_and_atomic_ceiling() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        let worker = std::thread::spawn(move || {
            for index in 0..2 {
                let mut request = server
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap()
                    .expect("provider request");
                assert_eq!(
                    request
                        .headers()
                        .iter()
                        .find(|header| header.field.equiv("authorization"))
                        .map(|header| header.value.as_str()),
                    Some("Bearer test-token")
                );
                if index == 0 {
                    assert_eq!(
                        request.url(),
                        "/v1/runtime/tools/scrapecreators.instagram.user.posts"
                    );
                    request
                        .respond(tiny_http::Response::from_string(
                            json!({
                                "ok":true,"data":{
                                    "slug":POSTS_TOOL,"effect":"read",
                                    "chargePrecondition":"max_charge_minor@1",
                                    "price":{"currency":"USD","scale":6,"amount":"0.002000"},
                                    "unitPrice":null
                                }
                            })
                            .to_string(),
                        ))
                        .unwrap();
                } else {
                    assert_eq!(request.url(), CALL_PATH);
                    assert_eq!(
                        request
                            .headers()
                            .iter()
                            .find(|header| header.field.equiv("idempotency-key"))
                            .map(|header| header.value.as_str()),
                        Some("app-call-example")
                    );
                    let mut body = String::new();
                    request.as_reader().read_to_string(&mut body).unwrap();
                    let body: Value = serde_json::from_str(&body).unwrap();
                    assert_eq!(body["slug"], POSTS_TOOL);
                    assert_eq!(body["query"], json!({"handle":"juicysuite_crm"}));
                    assert_eq!(body["max_charge_minor"], 2000);
                    assert!(body.get("company").is_none());
                    request
                        .respond(tiny_http::Response::from_string(json!({
                            "ok":true,"data":{
                                "slug":POSTS_TOOL,"repeated":false,
                                "price":{"currency":"USD","scale":6,"amount":"0.002000"},
                                "result":{"success":true,"status":"ok","items":[{
                                    "id":"post-1","code":"AbCd123","created_at":"2026-09-27T00:00:00Z",
                                    "user":{"username":"juicysuite_crm","is_private":false},
                                    "caption":{"text":"Provider-owned caption"}
                                }]}
                            }
                        }).to_string()))
                        .unwrap();
                }
            }
        });
        let adapter =
            AgenticosExternalAdapter::with_deployment_pin(&base, Some(MANIFEST_PIN)).unwrap();
        let mut authority = authority();
        let quote = adapter
            .quote_app_capability(b"test-token", &authority["binding"])
            .unwrap();
        assert_eq!(quote.total_price_micros, 2000);
        authority["quote"] = serde_json::to_value(quote).unwrap();
        let result = adapter
            .execute_app_capability(b"test-token", &authority, &json!({}), "app-call-example")
            .unwrap();
        assert_eq!(
            result.result["posts"][0]["caption"],
            "Provider-owned caption"
        );
        assert!(result.asset.is_none());
        worker.join().unwrap();
    }
}
