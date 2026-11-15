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
/// `CADENCE_AGENTICOS_PROVIDER_URL` when the daemon is started.
const DEFAULT_BASE: &str = "https://api-v2.agenticos.hk";
const READ_PATH: &str = "/v1/runtime/tools/read";
const BODY_CAP: u64 = 2 * 1024 * 1024;
const TIMEOUT_SECS: u64 = 90;

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
                        Some("insufficient_scope" | "credential_revoked" | "not_a_read")
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
}

impl PlatformAdapter for AgenticosProviderAdapter {
    fn table(&self) -> &ToolTable {
        &self.table
    }

    fn reported_manifest_version(&self) -> Option<String> {
        self.table.manifest_version.clone()
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
}
