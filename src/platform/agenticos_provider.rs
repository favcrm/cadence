//! CAD-1974: read-only marketing provider tools through the AgenticOS v2
//! connected-read door.
//!
//! The door (`POST /v1/runtime/tools/read`) admits, prices, claims and
//! settles the read itself; this adapter is only the outward leg. It holds no
//! price, no ledger and no provider credential of its own — the enrolled
//! `agenticos-provider` account is a device bearer with `provider.read`, and
//! every number this module returns is the platform's.
//!
//! A call here is never unattended: the table marks the tool `connected_read`,
//! so the gate requires the caller's connection and ceiling before this module
//! is reached, and [`PlatformAdapter::execute_for_account`] refuses outright.

use serde_json::Value;

use super::adapter::{AppCapabilityError, ConnectedRead, PlatformAdapter};
use crate::contract_fixture::ToolTable;
use crate::error::{Error, Result};

pub const PLATFORM: &str = "agenticos-provider";
/// Where a self-hosted or hosted AgenticOS v2 Worker answers. Overridden by
/// `CADENCE_AGENTICOS_PROVIDER_URL` when the daemon is started for legacy app tools.
const DEFAULT_BASE: &str = "https://api-v2.agenticos.hk";
pub const PRODUCTION_ORIGIN: &str = DEFAULT_BASE;
pub const SCOPE_DEDICATED_READ: &str = "provider.read.dedicated";
pub const SLUG_SEARCH_CONSOLE_REPORT: &str = "search_console_report";
pub const MANIFEST_VERSION: &str = "cadence-provider-tools@1";
const READ_PATH: &str = "/v1/runtime/tools/read";
pub const READ_ENDPOINT_URL: &str = "https://api-v2.agenticos.hk/v1/runtime/tools/read";
pub const LOOKUP_PATH: &str = "/v1/runtime/tools/requests/lookup";
const BODY_CAP: u64 = 2 * 1024 * 1024;
const TIMEOUT_SECS: u64 = 90;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DedicatedReadPrice {
    pub amount: String,
    pub amount_micros: u64,
    pub currency: String,
    pub scale: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DedicatedReadReply {
    pub repeated: bool,
    pub price: Option<DedicatedReadPrice>,
    pub result_json: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DedicatedReadLookup {
    pub outcome: String,
    pub recorded: bool,
    pub status: Option<String>,
    pub billing: Option<Value>,
}

pub fn parse_usd_micros(price: &Value) -> Option<(String, u64)> {
    if !valid_settled_price(price) {
        return None;
    }
    let amount = price["amount"].as_str()?;
    let (whole, fraction) = amount.split_once('.')?;
    let whole_num: u64 = whole.parse().ok()?;
    let fraction_num: u64 = fraction.parse().ok()?;
    let micros = whole_num
        .checked_mul(1_000_000)?
        .checked_add(fraction_num)?;
    if micros > cadence_core::connection_dispatch::primitives::I64_MAX_U64 {
        return None;
    }
    Some((amount.to_string(), micros))
}

fn screen_response_for_bearer(token: &str, bytes: &[u8], parsed: Option<&Value>) -> bool {
    let text = String::from_utf8_lossy(bytes);
    if crate::platform::refuse_leak("agenticos provider response", &text, token.as_bytes()).is_err()
    {
        return true;
    }
    let Ok(known) = cadence_core::connection_dispatch::KnownSecretV1::new(token.as_bytes(), &[])
    else {
        return true;
    };
    if known.screen_text(&text).is_err() {
        return true;
    }
    if let Some(value) = parsed {
        if known.screen_json(value).is_err() {
            return true;
        }
    }
    false
}

fn valid_settled_price(price: &Value) -> bool {
    let Some(fields) = price.as_object() else {
        return false;
    };
    if fields.len() != 3 || price["currency"] != "USD" || price["scale"].as_u64() != Some(6) {
        return false;
    }
    let Some((whole, fraction)) = price["amount"].as_str().and_then(|s| s.split_once('.')) else {
        return false;
    };
    !whole.is_empty()
        && whole.len() <= 10
        && whole.bytes().all(|byte| byte.is_ascii_digit())
        && (whole == "0" || !whole.starts_with('0'))
        && fraction.len() == 6
        && fraction.bytes().all(|byte| byte.is_ascii_digit())
}

/// Validate the platform's first-execution and sparse replay receipt shapes.
/// The settled price is checked as protocol data, never against a spending cap.
pub fn valid_connected_read_receipt(envelope: &Value) -> bool {
    if envelope["ok"] != true {
        return false;
    }
    let data = &envelope["data"];
    let (Some(repeated), Some(result)) = (data["repeated"].as_bool(), data.get("result")) else {
        return false;
    };
    let price = data.get("price");
    let valid_price = price.is_some_and(valid_settled_price);
    if data.get("effect").is_some_and(|effect| effect != "read")
        || (price.is_some() && !valid_price)
    {
        return false;
    }
    !result.is_null() && (repeated || (data["effect"] == "read" && valid_price))
}

/// Only operations AgenticOS v2 has qualified are declared. A provider whose
/// operation is not qualified is absent here, so this listing never promises a
/// call the platform would answer `unavailable`.
const TABLE: &str = r#"{"platform":"agenticos-provider","manifest_version":"cadence-provider-tools@1","tools":[
 {"tool":"search_console_report","effect":"read","scopes":["provider.read"],"connected_read":true,"label":"Read a Google Search Console performance report for one site and date range"}
]}"#;

pub struct AgenticosProviderAdapter {
    table: ToolTable,
    base: String,
    http: ureq::Agent,
}

impl AgenticosProviderAdapter {
    pub fn new(base: &str) -> Result<Self> {
        let uri: ureq::http::Uri = base
            .trim_end_matches('/')
            .parse()
            .map_err(|_| Error::rejected("AgenticOS provider origin is invalid"))?;
        let loopback = uri
            .host()
            .is_some_and(|host| matches!(host, "127.0.0.1" | "localhost" | "[::1]"));
        if !matches!(uri.scheme_str(), Some("https"))
            && !(uri.scheme_str() == Some("http") && loopback)
        {
            return Err(Error::rejected(
                "AgenticOS provider origin must be HTTPS (plain HTTP only on loopback)",
            ));
        }
        if uri.authority().is_none()
            || uri
                .authority()
                .is_some_and(|authority| authority.as_str().contains('@'))
            || !matches!(uri.path(), "" | "/")
            || uri.query().is_some()
        {
            return Err(Error::rejected(
                "AgenticOS provider origin must be an authority without credentials, path or query",
            ));
        }
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(TIMEOUT_SECS)))
            .max_redirects(0)
            .http_status_as_error(false)
            .build();
        Ok(Self {
            table: ToolTable::from_json(
                &serde_json::from_str(TABLE).expect("provider tool table parses"),
            )?,
            base: base.trim_end_matches('/').to_string(),
            http: ureq::Agent::new_with_config(config),
        })
    }

    /// One connected read. Preserve the platform's receipt, and distinguish
    /// pre-dispatch refusals from answers that cannot prove the outcome.
    fn read(
        &self,
        credential: &[u8],
        tool: &str,
        input: &Value,
        connection: &str,
        max_charge_minor: u64,
        idempotency_key: &str,
    ) -> std::result::Result<Value, AppCapabilityError> {
        let token = std::str::from_utf8(credential)
            .map_err(|_| {
                AppCapabilityError::Refused(
                    "the enrolled AgenticOS provider credential is not UTF-8".into(),
                )
            })?
            .trim();
        if token.is_empty() {
            return Err(AppCapabilityError::Refused(
                "the enrolled AgenticOS provider credential is empty".into(),
            ));
        }
        if connection.is_empty() {
            return Err(AppCapabilityError::Refused(
                "a connected read needs the connection it reads".into(),
            ));
        }
        if idempotency_key.is_empty() {
            return Err(AppCapabilityError::Refused(
                "a connected read needs an idempotency key".into(),
            ));
        }
        let body = serde_json::json!({
            "slug": tool,
            "connection_id": connection,
            "input": input,
            "max_charge_minor": max_charge_minor,
        });
        let url = format!("{}{READ_PATH}", self.base);
        let mut response = self
            .http
            .post(&url)
            .header("authorization", format!("Bearer {token}"))
            .header("idempotency-key", idempotency_key)
            .header("content-type", "application/json")
            .send(body.to_string())
            .map_err(|_| {
                AppCapabilityError::Uncertain(
                    "the AgenticOS provider request could not be confirmed".into(),
                )
            })?;
        let status = response.status().as_u16();
        let bytes = response
            .body_mut()
            .with_config()
            .limit(BODY_CAP)
            .read_to_vec()
            .map_err(|_| {
                AppCapabilityError::Uncertain(
                    "the AgenticOS provider answer could not be read within the supported bound"
                        .into(),
                )
            })?;
        let envelope: Value = serde_json::from_slice(&bytes).map_err(|_| {
            AppCapabilityError::Uncertain("the AgenticOS provider answer is not JSON".into())
        })?;
        if status == 200 && envelope["ok"] == true {
            if valid_connected_read_receipt(&envelope) {
                return Ok(envelope);
            }
            return Err(AppCapabilityError::Uncertain(
                "the AgenticOS provider answer has no confirmable receipt".into(),
            ));
        }
        Self::classify_error_envelope(status, &envelope)
    }

    fn classify_error_envelope(
        status: u16,
        envelope: &Value,
    ) -> std::result::Result<Value, AppCapabilityError> {
        let code = envelope["error"]["code"].as_str().filter(|code| {
            !code.is_empty()
                && code.len() <= 64
                && code
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        });
        let message = envelope["error"]["message"]
            .as_str()
            .filter(|message| !message.is_empty() && message.len() <= 400)
            .unwrap_or("the platform did not confirm the read");
        // These status/code pairs are produced before provider dispatch by the
        // connected-read door. In-progress, uncertain and upstream failures
        // can instead follow a funded attempt, regardless of HTTP status.
        let definite = envelope["ok"] == false
            && matches!(
                (status, code),
                (400, Some("invalid_request" | "quote_required"))
                    | (401, Some("unauthorized"))
                    | (402, Some("insufficient_funds"))
                    | (
                        403,
                        Some(
                            "insufficient_scope"
                                | "credential_revoked"
                                | "not_a_read"
                                | "dedicated_route_unconfigured"
                                | "workspace_mismatch"
                                | "deployment_mismatch"
                        )
                    )
                    | (404, Some("not_allowlisted" | "foreign_connection"))
                    | (
                        409,
                        Some(
                            "unavailable"
                                | "toolkit_disabled"
                                | "connection_inactive"
                                | "disabled"
                                | "unpriced"
                                | "price_limit"
                                | "price_changed"
                                | "not_configured"
                                | "unsupported_currency"
                        )
                    )
                    | (503, Some("capability_unavailable"))
            );
        let detail = format!(
            "AgenticOS provider {}: {}: {message}",
            if definite {
                "refused the read"
            } else {
                "outcome is unconfirmed"
            },
            code.unwrap_or("unconfirmed"),
        );
        Err(if definite {
            AppCapabilityError::Refused(detail)
        } else {
            AppCapabilityError::Uncertain(detail)
        })
    }

    /// One protected dedicated read with frozen request bytes (never `max_charge_minor`),
    /// idempotency key, price parsing, and bearer-leak screening.
    pub fn execute_dedicated_read(
        &self,
        credential: &[u8],
        frozen_body: &[u8],
        idempotency_key: &str,
    ) -> std::result::Result<DedicatedReadReply, AppCapabilityError> {
        let token = std::str::from_utf8(credential)
            .map_err(|_| {
                AppCapabilityError::Refused(
                    "the enrolled AgenticOS provider credential is not UTF-8".into(),
                )
            })?
            .trim();
        if token.is_empty() || token.chars().any(char::is_control) {
            return Err(AppCapabilityError::Refused(
                "the enrolled AgenticOS provider credential is invalid".into(),
            ));
        }
        if !(8..=128).contains(&idempotency_key.len())
            || !idempotency_key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(AppCapabilityError::Refused(
                "a dedicated read needs a valid idempotency key".into(),
            ));
        }
        let body_value: Value = serde_json::from_slice(frozen_body).map_err(|_| {
            AppCapabilityError::Refused("the frozen dedicated read body is malformed".into())
        })?;
        let Some(body_map) = body_value.as_object() else {
            return Err(AppCapabilityError::Refused(
                "the frozen dedicated read body must be a JSON object".into(),
            ));
        };
        if body_map.contains_key("max_charge_minor") {
            return Err(AppCapabilityError::Refused(
                "dedicated reads do not accept max_charge_minor".into(),
            ));
        }
        let url = format!("{}{READ_PATH}", self.base);
        let mut response = self
            .http
            .post(&url)
            .header("authorization", format!("Bearer {token}"))
            .header("idempotency-key", idempotency_key)
            .header("content-type", "application/json")
            .send(frozen_body)
            .map_err(|_| {
                AppCapabilityError::Uncertain(
                    "the AgenticOS provider request could not be confirmed".into(),
                )
            })?;
        let status = response.status().as_u16();
        let bytes = response
            .body_mut()
            .with_config()
            .limit(BODY_CAP)
            .read_to_vec()
            .map_err(|_| {
                AppCapabilityError::Uncertain(
                    "the AgenticOS provider answer could not be read within the supported bound"
                        .into(),
                )
            })?;
        if screen_response_for_bearer(token, &bytes, None) {
            return Err(AppCapabilityError::Uncertain(
                "the AgenticOS provider answer was withheld".into(),
            ));
        }
        let envelope: Value = serde_json::from_slice(&bytes).map_err(|_| {
            AppCapabilityError::Uncertain("the AgenticOS provider answer is not JSON".into())
        })?;
        if screen_response_for_bearer(token, &bytes, Some(&envelope)) {
            return Err(AppCapabilityError::Uncertain(
                "the AgenticOS provider answer was withheld".into(),
            ));
        }
        if status == 200 && envelope["ok"] == true {
            if !valid_connected_read_receipt(&envelope) {
                return Err(AppCapabilityError::Uncertain(
                    "the AgenticOS provider answer has no confirmable receipt".into(),
                ));
            }
            let data = &envelope["data"];
            let repeated = data["repeated"].as_bool().ok_or_else(|| {
                AppCapabilityError::Uncertain(
                    "the AgenticOS provider answer has no confirmable receipt".into(),
                )
            })?;
            let price = match data.get("price") {
                Some(raw_price) => {
                    let (amount, amount_micros) = parse_usd_micros(raw_price).ok_or_else(|| {
                        AppCapabilityError::Uncertain(
                            "the AgenticOS provider answer has an unreadable price".into(),
                        )
                    })?;
                    Some(DedicatedReadPrice {
                        amount,
                        amount_micros,
                        currency: "USD".to_string(),
                        scale: 6,
                    })
                }
                None => None,
            };
            let canonical_result =
                cadence_core::connection_dispatch::canonical_value(&data["result"]);
            let result_json = serde_json::to_string(&canonical_result).map_err(|_| {
                AppCapabilityError::Uncertain(
                    "the AgenticOS provider answer result did not serialize".into(),
                )
            })?;
            if result_json.is_empty()
                || result_json.len()
                    > cadence_core::connection_dispatch::dto::MAX_AGENTICOS_READ_BYTES
            {
                return Err(AppCapabilityError::Uncertain(
                    "the AgenticOS provider answer result exceeds the supported bound".into(),
                ));
            }
            if screen_response_for_bearer(token, result_json.as_bytes(), Some(&canonical_result)) {
                return Err(AppCapabilityError::Uncertain(
                    "the AgenticOS provider answer was withheld".into(),
                ));
            }
            return Ok(DedicatedReadReply {
                repeated,
                price,
                result_json,
            });
        }
        Err(Self::classify_error_envelope(status, &envelope).unwrap_err())
    }

    /// Read-only reconciliation lookup against `/v1/runtime/tools/requests/lookup`.
    /// Never issues a POST or retries an execution.
    pub fn lookup_dedicated_read(
        &self,
        credential: &[u8],
        idempotency_key: &str,
        slug: &str,
    ) -> std::result::Result<DedicatedReadLookup, AppCapabilityError> {
        let token = std::str::from_utf8(credential)
            .map_err(|_| {
                AppCapabilityError::Refused(
                    "the enrolled AgenticOS provider credential is not UTF-8".into(),
                )
            })?
            .trim();
        if token.is_empty() || token.chars().any(char::is_control) {
            return Err(AppCapabilityError::Refused(
                "the enrolled AgenticOS provider credential is invalid".into(),
            ));
        }
        if idempotency_key.is_empty()
            || idempotency_key.len() > 128
            || !idempotency_key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(AppCapabilityError::Refused(
                "a dedicated read lookup needs a valid idempotency key".into(),
            ));
        }
        if slug.is_empty()
            || slug.len() > 64
            || !slug
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(AppCapabilityError::Refused(
                "a dedicated read lookup needs a valid tool slug".into(),
            ));
        }
        let url = format!(
            "{}{LOOKUP_PATH}?key={idempotency_key}&slug={slug}",
            self.base
        );
        let mut response = self
            .http
            .get(&url)
            .header("authorization", format!("Bearer {token}"))
            .call()
            .map_err(|_| {
                AppCapabilityError::Uncertain(
                    "the AgenticOS provider lookup could not be confirmed".into(),
                )
            })?;
        let status = response.status().as_u16();
        let bytes = response
            .body_mut()
            .with_config()
            .limit(BODY_CAP)
            .read_to_vec()
            .map_err(|_| {
                AppCapabilityError::Uncertain(
                    "the AgenticOS provider lookup answer exceeded the supported bound".into(),
                )
            })?;
        if screen_response_for_bearer(token, &bytes, None) {
            return Err(AppCapabilityError::Uncertain(
                "the AgenticOS provider lookup answer was withheld".into(),
            ));
        }
        let envelope: Value = serde_json::from_slice(&bytes).map_err(|_| {
            AppCapabilityError::Uncertain("the AgenticOS provider lookup answer is not JSON".into())
        })?;
        if screen_response_for_bearer(token, &bytes, Some(&envelope)) {
            return Err(AppCapabilityError::Uncertain(
                "the AgenticOS provider lookup answer was withheld".into(),
            ));
        }
        if status != 200 || envelope["ok"] != true {
            return Err(AppCapabilityError::Uncertain(
                "the AgenticOS provider lookup did not confirm an outcome".into(),
            ));
        }
        let data = &envelope["data"];
        let outcome = data["outcome"]
            .as_str()
            .filter(|v| matches!(*v, "unrecorded" | "recorded" | "unknown"))
            .ok_or_else(|| {
                AppCapabilityError::Uncertain(
                    "the AgenticOS provider lookup outcome is unrecognized".into(),
                )
            })?;
        let recorded = data["recorded"].as_bool().ok_or_else(|| {
            AppCapabilityError::Uncertain(
                "the AgenticOS provider lookup recorded flag is missing".into(),
            )
        })?;
        let lookup_status = data["status"]
            .as_str()
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 64
                    && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            })
            .map(str::to_string);
        let billing = data.get("billing").filter(|v| !v.is_null()).cloned();
        Ok(DedicatedReadLookup {
            outcome: outcome.to_string(),
            recorded,
            status: lookup_status,
            billing,
        })
    }
}

impl PlatformAdapter for AgenticosProviderAdapter {
    fn table(&self) -> &ToolTable {
        &self.table
    }

    fn reported_manifest_version(&self) -> Option<String> {
        self.table.manifest_version.clone()
    }

    fn connection_descriptor(&self) -> Option<crate::platform::connections::ProviderDescriptor> {
        Some(crate::platform::connections::ProviderDescriptor {
            schema: 1,
            provider: PLATFORM.to_string(),
            revision: MANIFEST_VERSION.to_string(),
            enrollment_shapes: vec!["token".to_string()],
            builtin_accounts: vec![],
            capabilities: vec![crate::platform::connections::CapabilityDescriptor {
                id: "search_console.read".to_string(),
                version: 1,
                tools: vec![SLUG_SEARCH_CONSOLE_REPORT.to_string()],
                scopes: vec!["provider.read".to_string()],
                effect: "read".to_string(),
                semantics: crate::platform::connections::CapabilitySemantics::MetadataRead,
            }],
            action_mappings: vec![],
        })
    }

    fn preview(&self, account: &str, tool: &str, _input: &Value) -> String {
        format!("{tool} through agenticos-provider/{account}")
    }

    /// A connected read has no unattended path: without the caller's
    /// connection and ceiling there is nothing to run.
    fn execute(
        &self,
        _credential: &[u8],
        _tool: &str,
        _input: &Value,
        _idempotency_key: &str,
        _expected_hash: Option<&str>,
    ) -> std::result::Result<Value, String> {
        Err("AgenticOS provider reads need --connection and --max-charge".into())
    }

    fn execute_connected_read(
        &self,
        _account: &str,
        credential: &[u8],
        call: &ConnectedRead<'_>,
    ) -> std::result::Result<Value, AppCapabilityError> {
        if !self
            .table
            .declared(call.tool)
            .is_some_and(|decl| decl.connected_read)
        {
            return Err(AppCapabilityError::Refused(
                "this tool is not a reviewed connected read".into(),
            ));
        }
        self.read(
            credential,
            call.tool,
            call.input,
            call.connection,
            call.max_charge_minor,
            call.idempotency_key,
        )
    }

    fn read_back(&self, _tool: &str, _input: &Value) -> crate::contract_fixture::Verified {
        crate::contract_fixture::Verified::Unknown
    }

    fn source_hash(&self, _agent: &str, _source: &str) -> Option<String> {
        None
    }
}

/// Register the adapter when the daemon starts. Always available: a call with
/// no enrolled `agenticos-provider` account refuses at the credential step,
/// which is a clearer answer than "no adapter for this platform".
pub fn attach(opts: &mut crate::daemon::ServeOptions) -> Result<()> {
    if opts.platforms.contains_key(PLATFORM) {
        return Ok(());
    }
    let base = std::env::var("CADENCE_AGENTICOS_PROVIDER_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_BASE.to_string());
    // A malformed override is a startup refusal, never a silent fallback to a
    // different origin.
    let adapter = AgenticosProviderAdapter::new(&base)?;
    opts.platforms
        .insert(PLATFORM.into(), std::sync::Arc::new(adapter));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct DoorRequest {
        method: String,
        url: String,
        auth: Option<String>,
        idem: Option<String>,
        body: Value,
    }

    fn door(
        reply: Value,
        status: u16,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<DoorRequest>>>,
        std::thread::JoinHandle<()>,
    ) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = seen.clone();
        let worker = std::thread::spawn(move || {
            let mut first = true;
            loop {
                let window = if first {
                    std::time::Duration::from_secs(10)
                } else {
                    std::time::Duration::from_millis(400)
                };
                first = false;
                let Some(mut request) = server.recv_timeout(window).unwrap() else {
                    break;
                };
                let mut text = String::new();
                request.as_reader().read_to_string(&mut text).unwrap();
                let observed = DoorRequest {
                    method: request.method().to_string(),
                    url: request.url().to_string(),
                    auth: request
                        .headers()
                        .iter()
                        .find(|header| header.field.equiv("authorization"))
                        .map(|header| header.value.as_str().to_owned()),
                    idem: request
                        .headers()
                        .iter()
                        .find(|header| header.field.equiv("idempotency-key"))
                        .map(|header| header.value.as_str().to_owned()),
                    body: serde_json::from_str(&text).unwrap_or(Value::Null),
                };
                record.lock().unwrap().push(observed);
                let payload = tiny_http::Response::from_string(reply.to_string())
                    .with_status_code(status)
                    .with_header(
                        tiny_http::Header::from_bytes(
                            &b"content-type"[..],
                            &b"application/json"[..],
                        )
                        .unwrap(),
                    );
                request.respond(payload).unwrap();
            }
        });
        (base, seen, worker)
    }

    fn input() -> Value {
        json!({"site_url": "sc-domain:example.test", "start_date": "2026-10-01", "end_date": "2026-10-01"})
    }

    #[test]
    fn the_table_declares_only_qualified_reads() {
        let adapter = AgenticosProviderAdapter::new("https://api-v2.agenticos.hk").unwrap();
        let table = adapter.table();
        assert_eq!(
            table.manifest_version.as_deref(),
            Some("cadence-provider-tools@1")
        );
        assert!(table
            .tools
            .iter()
            .all(|decl| decl.effect.as_deref() == Some("read")));
        assert!(table.tools.iter().all(|decl| decl.connected_read));
        assert_eq!(
            table.tools.len(),
            1,
            "an unqualified provider must not be listed"
        );
        assert_eq!(table.tools[0].tool, "search_console_report");
        assert!(adapter.direct_call("search_console_report"));
    }

    #[test]
    fn a_read_posts_the_platform_contract_and_returns_its_own_outcome() {
        let outcome = json!({"ok": true, "data": {
            "effect": "read",
            "price": {"currency": "USD", "scale": 6, "amount": "0.000025"},
            "result": {"rows": [{"keys": ["example"], "clicks": 3}]},
            "repeated": false
        }});
        let (base, seen, worker) = door(outcome.clone(), 200);
        let adapter = AgenticosProviderAdapter::new(&base).unwrap();
        let out = adapter
            .execute_connected_read(
                "prod",
                b"hct_test_token",
                &ConnectedRead {
                    tool: "search_console_report",
                    input: &input(),
                    connection: "con_alpha",
                    max_charge_minor: 25,
                    idempotency_key: "req-1",
                },
            )
            .unwrap();
        assert_eq!(out, outcome);
        worker.join().unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "exactly one provider call");
        let call = &seen[0];
        assert_eq!(call.method, "POST");
        assert_eq!(call.url, "/v1/runtime/tools/read");
        assert_eq!(call.auth.as_deref(), Some("Bearer hct_test_token"));
        assert_eq!(call.idem.as_deref(), Some("req-1"));
        assert_eq!(call.body["slug"], "search_console_report");
        assert_eq!(call.body["connection_id"], "con_alpha");
        assert_eq!(call.body["max_charge_minor"], 25);
        assert_eq!(call.body["input"], input());
    }

    #[test]
    fn a_replay_without_a_result_is_uncertain_and_is_not_retried() {
        let (base, seen, worker) = door(
            json!({"ok": true, "data": {"repeated": true, "result": null}}),
            200,
        );
        let adapter = AgenticosProviderAdapter::new(&base).unwrap();
        let error = adapter
            .execute_connected_read(
                "prod",
                b"hct_test_token",
                &ConnectedRead {
                    tool: "search_console_report",
                    input: &input(),
                    connection: "con_alpha",
                    max_charge_minor: 25,
                    idempotency_key: "req-missing-result",
                },
            )
            .unwrap_err();
        worker.join().unwrap();
        assert!(
            matches!(&error, AppCapabilityError::Uncertain(_)),
            "{error}"
        );
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn the_platforms_refusal_is_repeated_verbatim() {
        let (base, _seen, worker) = door(
            json!({"ok": false, "error": {"code": "unavailable", "message": "This tool is not available."}}),
            409,
        );
        let adapter = AgenticosProviderAdapter::new(&base).unwrap();
        let error = adapter
            .execute_connected_read(
                "prod",
                b"hct_test_token",
                &ConnectedRead {
                    tool: "search_console_report",
                    input: &input(),
                    connection: "con_alpha",
                    max_charge_minor: 25,
                    idempotency_key: "req-2",
                },
            )
            .unwrap_err();
        worker.join().unwrap();
        assert!(error.reason().contains("unavailable"), "{error}");
        assert!(
            error.reason().contains("This tool is not available."),
            "{error}"
        );
    }

    #[test]
    fn an_unenrolled_tool_and_an_unattended_call_both_refuse() {
        let adapter = AgenticosProviderAdapter::new("https://api-v2.agenticos.hk").unwrap();
        let error = adapter
            .execute_connected_read(
                "prod",
                b"token",
                &ConnectedRead {
                    tool: "analytics_report",
                    input: &json!({}),
                    connection: "con_alpha",
                    max_charge_minor: 25,
                    idempotency_key: "req-3",
                },
            )
            .unwrap_err();
        assert!(
            error.reason().contains("not a reviewed connected read"),
            "{error}"
        );
        let error = adapter
            .execute(b"token", "search_console_report", &input(), "req-4", None)
            .unwrap_err();
        assert!(error.contains("--connection"), "{error}");
    }

    #[test]
    fn an_http_origin_is_only_accepted_on_loopback() {
        assert!(AgenticosProviderAdapter::new("http://api-v2.agenticos.hk").is_err());
        assert!(AgenticosProviderAdapter::new("http://127.0.0.1:8787").is_ok());
        assert!(AgenticosProviderAdapter::new("https://api-v2.agenticos.hk/some/path").is_err());
        assert!(AgenticosProviderAdapter::new("https://user:pw@api-v2.agenticos.hk").is_err());
    }

    #[test]
    fn dedicated_read_omits_max_charge_minor_and_parses_price() {
        let secret = b"tok_supersecret_abc123";
        let adapter_no_http = AgenticosProviderAdapter::new("http://127.0.0.1:9").unwrap();
        let bad_body = serde_json::to_vec(&json!({
            "slug": "search_console_report",
            "connection_id": "con_alpha",
            "input": input(),
            "max_charge_minor": 10,
        }))
        .unwrap();
        let refused = adapter_no_http
            .execute_dedicated_read(secret, &bad_body, "req-dedicated-bad")
            .unwrap_err();
        assert!(
            matches!(refused, AppCapabilityError::Refused(_)),
            "{refused}"
        );

        let (base, seen, worker) = door(
            json!({
                "ok": true,
                "data": {
                    "effect": "read",
                    "repeated": false,
                    "price": {"amount": "0.000003", "currency": "USD", "scale": 6},
                    "result": {"rows": [{"clicks": 2}], "dimension": null}
                }
            }),
            200,
        );
        let adapter = AgenticosProviderAdapter::new(&base).unwrap();
        let frozen_body = serde_json::to_vec(&json!({
            "slug": "search_console_report",
            "connection_id": "con_alpha",
            "input": input(),
        }))
        .unwrap();
        let reply = adapter
            .execute_dedicated_read(secret, &frozen_body, "req-dedicated-1")
            .unwrap();
        worker.join().unwrap();
        let recorded = seen.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        let call = &recorded[0];
        assert_eq!(call.method, "POST");
        assert_eq!(call.url, "/v1/runtime/tools/read");
        assert_eq!(call.auth.as_deref(), Some("Bearer tok_supersecret_abc123"));
        assert_eq!(call.idem.as_deref(), Some("req-dedicated-1"));
        assert!(call.body.get("max_charge_minor").is_none());
        assert!(!reply.repeated);
        let price = reply.price.expect("price");
        assert_eq!(price.amount, "0.000003");
        assert_eq!(price.amount_micros, 3);
        assert_eq!(price.currency, "USD");
        assert_eq!(price.scale, 6);
        assert!(
            reply.result_json.contains("\"clicks\":2"),
            "{}",
            reply.result_json
        );
    }

    #[test]
    fn dedicated_read_withholds_bearer_leak_as_uncertain() {
        let secret = b"tok_supersecret_abc123";
        let (base, seen, worker) = door(
            json!({
                "ok": true,
                "data": {
                    "effect": "read",
                    "repeated": false,
                    "price": {"amount": "0.000003", "currency": "USD", "scale": 6},
                    "result": {"leaked": "echo supersec fragment"}
                }
            }),
            200,
        );
        let adapter = AgenticosProviderAdapter::new(&base).unwrap();
        let frozen_body = serde_json::to_vec(&json!({
            "slug": "search_console_report",
            "connection_id": "con_alpha",
            "input": input(),
        }))
        .unwrap();
        let err = adapter
            .execute_dedicated_read(secret, &frozen_body, "req-dedicated-leak")
            .unwrap_err();
        worker.join().unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(matches!(err, AppCapabilityError::Uncertain(_)), "{err}");
        assert!(!err.reason().contains("supersec"), "{err}");
    }

    #[test]
    fn dedicated_read_lookup_reconciles_without_post() {
        let secret = b"tok_supersecret_abc123";
        let (base, seen, worker) = door(
            json!({
                "ok": true,
                "data": {
                    "outcome": "recorded",
                    "recorded": true,
                    "key": "req-dedicated-2",
                    "tool": "search_console_report",
                    "status": "settled",
                    "billing": {"amount_minor": 3}
                }
            }),
            200,
        );
        let adapter = AgenticosProviderAdapter::new(&base).unwrap();
        let lookup = adapter
            .lookup_dedicated_read(secret, "req-dedicated-2", SLUG_SEARCH_CONSOLE_REPORT)
            .unwrap();
        worker.join().unwrap();
        let recorded = seen.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].method, "GET");
        assert_eq!(
            recorded[0].url,
            "/v1/runtime/tools/requests/lookup?key=req-dedicated-2&slug=search_console_report"
        );
        assert_eq!(
            recorded[0].auth.as_deref(),
            Some("Bearer tok_supersecret_abc123")
        );
        assert_eq!(lookup.outcome, "recorded");
        assert!(lookup.recorded);
        assert_eq!(lookup.status.as_deref(), Some("settled"));
        assert_eq!(lookup.billing, Some(json!({"amount_minor": 3})));
    }

    #[test]
    fn parse_usd_micros_handles_valid_and_invalid_prices() {
        assert_eq!(
            parse_usd_micros(&json!({"amount": "0.000003", "currency": "USD", "scale": 6})),
            Some(("0.000003".to_string(), 3))
        );
        assert_eq!(
            parse_usd_micros(&json!({"amount": "1.500000", "currency": "USD", "scale": 6})),
            Some(("1.500000".to_string(), 1_500_000))
        );
        assert_eq!(
            parse_usd_micros(&json!({"amount": "0.000000", "currency": "USD", "scale": 6})),
            Some(("0.000000".to_string(), 0))
        );
        for bad in [
            json!({"amount": "01.000000", "currency": "USD", "scale": 6}),
            json!({"amount": "-0.000003", "currency": "USD", "scale": 6}),
            json!({"amount": "0.003", "currency": "USD", "scale": 6}),
            json!({"amount": "0.000003", "currency": "EUR", "scale": 6}),
            json!({"amount": "0.000003", "currency": "USD", "scale": 2}),
        ] {
            assert_eq!(parse_usd_micros(&bad), None, "{bad}");
        }
    }

    #[test]
    fn connection_descriptor_validates_against_table() {
        let adapter = AgenticosProviderAdapter::new(PRODUCTION_ORIGIN).unwrap();
        let descriptor = adapter
            .connection_descriptor()
            .expect("agenticos-provider exposes a reviewed descriptor");
        descriptor.validate(adapter.table()).unwrap();
        assert_eq!(descriptor.revision, MANIFEST_VERSION);
        assert_eq!(descriptor.enrollment_shapes, vec!["token".to_string()]);
        assert_eq!(
            descriptor.capabilities[0].tools,
            vec![SLUG_SEARCH_CONSOLE_REPORT.to_string()]
        );
    }
}
