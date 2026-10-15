//! CAD-1423: the native client for the platform's Web Push host. Both calls go
//! to fixed private `api.internal` runtime doors. No URL, cookie, credential
//! or signing key comes from the caller. Every response is bounded and
//! strictly parsed; anything else is a refusal the caller fails closed on.

use std::time::Duration;

use serde_json::{json, Value};

use crate::error::{Error, Result};

const CONFIG_URL: &str = "http://api.internal/v1/runtime/cadence/push/config";
const DELIVERY_URL: &str = "http://api.internal/v1/runtime/cadence/push/delivery";
/// Config is one short read. A delivery invoke covers the platform's prepare,
/// provider send and finish, so it gets its own bound (the lease is 30 s).
const CONFIG_DEADLINE: Duration = Duration::from_secs(5);
const DELIVERY_DEADLINE: Duration = Duration::from_secs(20);
const RESPONSE_MAX: u64 = 4096;

/// The platform's public Web Push configuration. `public_key` is present only
/// when push is available.
pub struct PushConfig {
    pub available: bool,
    pub public_key: Option<String>,
}

fn agent(deadline: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(deadline))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into()
}

fn unavailable() -> Error {
    Error::invalid(
        "push_unavailable",
        "web push is unavailable on this platform",
    )
}

fn refused() -> Error {
    Error::invalid("push_refused", "the platform refused the push request")
}

/// The `data` of a strict, closed `{ok:true,data:{...}}` envelope. Any other
/// root key, a missing key or a non-object root is refused.
fn ok_data(bytes: &[u8]) -> Option<serde_json::Map<String, Value>> {
    let Value::Object(root) = serde_json::from_slice::<Value>(bytes).ok()? else {
        return None;
    };
    if root.len() != 2 || root.get("ok") != Some(&Value::Bool(true)) {
        return None;
    }
    match root.get("data") {
        Some(Value::Object(data)) => Some(data.clone()),
        _ => None,
    }
}

/// The data object must carry exactly `keys` and nothing else.
fn exact_keys(data: &serde_json::Map<String, Value>, keys: &[&str]) -> bool {
    data.len() == keys.len() && keys.iter().all(|k| data.contains_key(*k))
}

/// A Web Push application-server key: the exact 87-character base64url form of
/// an uncompressed P-256 point, validated with the existing `web_push` helper.
fn valid_key(k: &str) -> bool {
    k.len() == 87 && crate::web_push::valid_p256_point(k)
}

/// Read the platform's push configuration. Any failure is `push_unavailable`.
pub fn push_config() -> Result<PushConfig> {
    let mut resp = agent(CONFIG_DEADLINE)
        .get(CONFIG_URL)
        .call()
        .map_err(|_| unavailable())?;
    if resp.status() != 200 {
        return Err(unavailable());
    }
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(RESPONSE_MAX)
        .read_to_vec()
        .map_err(|_| unavailable())?;
    let data = ok_data(&bytes).ok_or_else(unavailable)?;
    if !exact_keys(&data, &["available", "public_key"]) {
        return Err(unavailable());
    }
    let key = match data.get("public_key") {
        Some(Value::String(k)) if valid_key(k) => Some(k.clone()),
        Some(Value::Null) => None,
        _ => return Err(unavailable()),
    };
    // Available and the key are coherent: a key with no availability, or
    // availability with no key, is a broken answer and fails closed.
    match (data.get("available"), key) {
        (Some(Value::Bool(true)), Some(k)) => Ok(PushConfig {
            available: true,
            public_key: Some(k),
        }),
        (Some(Value::Bool(false)), None) => Ok(PushConfig {
            available: false,
            public_key: None,
        }),
        _ => Err(unavailable()),
    }
}

/// Ask the platform to run one delivery for an opaque (intent, device) pair.
/// The response acknowledges the invocation only. Delivery and read state
/// are owned by the host's prepare and finish callbacks, never by this reply.
pub fn push_delivery_invoke(intent: i64, device: &str) -> Result<()> {
    let body = json!({"intent_id": intent, "device_id": device});
    let mut resp = agent(DELIVERY_DEADLINE)
        .post(DELIVERY_URL)
        .send_json(&body)
        .map_err(|_| refused())?;
    if resp.status() != 200 {
        return Err(refused());
    }
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(RESPONSE_MAX)
        .read_to_vec()
        .map_err(|_| refused())?;
    let data = ok_data(&bytes).ok_or_else(refused)?;
    if !exact_keys(&data, &["state"]) {
        return Err(refused());
    }
    match data.get("state").and_then(Value::as_str) {
        Some("accepted" | "gone" | "retry" | "suppressed" | "unavailable") => Ok(()),
        _ => Err(refused()),
    }
}
