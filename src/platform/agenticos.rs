//! CAD-501 / ADR 0006 §5.2: the AgenticOS platform adapter.
//!
//! Publish is a Cadence `draft`, not a second approval gate. AgenticOS
//! owns the send: `execute` posts to the runtime door
//! (`POST /v1/runtime/connectors/publish`) with an idempotency key and
//! the expected content hash, and the CompanyControl ledger answers
//! waiting, published (with a receipt), or refused. Cadence mirrors
//! that answer; it does not press again.
//!
//! Hosted containers call `http://api.internal` with no credential —
//! the host binds the company. A self-hosted daemon that has enrolled
//! a CAD-366 scoped token sends it as `Authorization: Bearer`. There
//! is no consent-exchange adapter: the merged contract (AgenticOS
//! PR #102) keeps OAuth on the AgenticOS API, not on this board.
//! Destination discovery and the Connections page stay with CAD-585.
//!
//! Image generation is not on this door, so this table does not
//! declare it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::contract_fixture::{ToolTable, Verified};
use crate::error::{Error, Result};
use crate::platform::adapter::PlatformAdapter;

/// The platform name `ServeOptions::platforms` registers this adapter
/// under — `platform_call {platform:"agenticos", …}` reaches it.
pub const PLATFORM: &str = "agenticos";

/// The account a hosted container uses. No enrollment: the host binds
/// the company, so custody holds no bytes (the same shape as
/// `local`/`local`).
pub const HOSTED_ACCOUNT: &str = "hosted";

/// Base URL inside a hosted container. `CADENCE_AGENTICOS_URL`
/// overrides it; a daemon that is not hosted does not register this
/// adapter at all.
pub const HOSTED_BASE: &str = "http://api.internal";

/// Where an operator opens the company that is waiting on the publish.
/// The runtime door has no per-send URL; this is the account app the
/// contract names.
const ACCOUNT_APP: &str = "https://app-v2.agenticos.hk/account";

const DOOR: &str = "/v1/runtime/connectors";

/// Reviewed pin. The runtime door does not report a manifest version,
/// so the adapter reports the version this table was reviewed against
/// — the same way `local` does.
const MANIFEST_VERSION: &str = "agenticos-connectors/1";

const TABLE_JSON: &str = r#"{
    "platform": "agenticos",
    "manifest_version": "agenticos-connectors/1",
    "tools": [
        {
            "tool": "connections_list",
            "effect": "read",
            "scopes": ["sources"],
            "label": "List connected Facebook Pages and Instagram accounts"
        },
        {
            "tool": "connection_profile",
            "effect": "read",
            "scopes": ["sources"],
            "label": "Read a connected account's profile"
        },
        {
            "tool": "connection_posts",
            "effect": "read",
            "scopes": ["sources"],
            "label": "Read recent posts on a connected account"
        },
        {
            "tool": "connection_insights",
            "effect": "read",
            "scopes": ["sources"],
            "label": "Read insight metrics for a connected account"
        },
        {
            "tool": "post_draft",
            "effect": "draft",
            "scopes": ["draft"],
            "label": "Preview a post. Nothing is sent to the provider"
        },
        {
            "tool": "publish_post",
            "effect": "draft",
            "scopes": ["publish"],
            "label": "Hand a post to AgenticOS for approval"
        }
    ]
}"#;

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const BODY_CAP: usize = 64 * 1024;
const CAPTION_CAP: usize = 8000;
const CONNECTION_CAP: usize = 80;
const MEDIA_CAP: usize = 200;

/// One publish the adapter has already handed off, so `read_back` can
/// re-check the receipt without inventing a second write.
struct Handoff {
    key: String,
    digest: String,
    verified: Verified,
}

/// The AgenticOS adapter. `base` is the runtime origin (`http://api.internal`
/// when hosted). Custody bytes, when present, are a scoped bearer token;
/// empty bytes are the hosted path.
pub struct AgenticosAdapter {
    table: ToolTable,
    base: String,
    http: ureq::Agent,
    handoffs: Mutex<HashMap<String, Handoff>>,
}

impl AgenticosAdapter {
    /// `base` is `http://` or `https://` with a host and no userinfo.
    /// A trailing slash is dropped.
    pub fn new(base: &str) -> Result<Self> {
        let base = check_base(base)?;
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(HTTP_TIMEOUT))
            .http_status_as_error(false)
            .max_redirects(0)
            .build();
        let table = ToolTable::from_json(&serde_json::from_str(TABLE_JSON).expect("table parses"))
            .expect("agenticos tool table parses");
        Ok(Self {
            table,
            base,
            http: ureq::Agent::new_with_config(config),
            handoffs: Mutex::new(HashMap::new()),
        })
    }
}

/// Register the adapter on a daemon that already decided the base URL.
pub fn register(opts: &mut crate::daemon::ServeOptions, base: &str) -> Result<()> {
    let adapter = AgenticosAdapter::new(base)?;
    opts.platforms
        .insert(PLATFORM.to_string(), std::sync::Arc::new(adapter));
    Ok(())
}

/// Hosted daemons (a real `hosted.lease`) and any daemon with
/// `CADENCE_AGENTICOS_URL` set speak to AgenticOS. Everyone else leaves
/// the platform unregistered — a call then fails closed. An explicit
/// URL wins over the hosted default. This does not read
/// `AGENTICOS_BOARD_COMPANY`: that variable is set on shared dev hosts
/// that are not the hosted container.
pub fn attach(opts: &mut crate::daemon::ServeOptions, hosted: &crate::lease::Hosted) -> Result<()> {
    if opts.platforms.contains_key(PLATFORM) {
        return Ok(());
    }
    let explicit = std::env::var("CADENCE_AGENTICOS_URL").ok();
    let Some(base) = resolve_base(lease_is_on(hosted), explicit.as_deref()) else {
        return Ok(());
    };
    register(opts, &base)
}

/// `Some` is the origin to register. An explicit URL wins, including
/// when the daemon is not hosted. Hosted with no override is
/// [`HOSTED_BASE`].
pub fn resolve_base(hosted_lease: bool, explicit_url: Option<&str>) -> Option<String> {
    if let Some(url) = explicit_url.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(url.trim_end_matches('/').to_string());
    }
    if hosted_lease {
        return Some(HOSTED_BASE.to_string());
    }
    None
}

fn lease_is_on(hosted: &crate::lease::Hosted) -> bool {
    matches!(
        hosted.lease.as_deref().map(str::trim),
        Some(s) if !s.is_empty() && s != "none" && s != "off"
    )
}

fn check_base(base: &str) -> Result<String> {
    let base = base.trim().trim_end_matches('/').to_string();
    let rest = base
        .strip_prefix("https://")
        .or_else(|| base.strip_prefix("http://"))
        .ok_or_else(|| Error::rejected("agenticos base URL must start with http:// or https://"))?;
    if rest.is_empty() || rest.contains('@') || rest.contains(' ') || rest.starts_with('/') {
        return Err(Error::rejected(
            "agenticos base URL needs a host and must not carry credentials",
        ));
    }
    Ok(base)
}

/// The digest AgenticOS records for a publish (`publishContentDigest`):
/// SHA-256 over the length-prefixed connection, caption and media key.
/// Lengths are UTF-16 code units, matching the JavaScript string length
/// the ledger uses. The hex is lowercase, without a `sha256:` prefix.
pub fn publish_content_digest(
    connection_id: &str,
    caption: &str,
    media_key: Option<&str>,
) -> String {
    let media = media_key.unwrap_or("");
    let canonical = format!(
        "{}:{connection_id}\n{}:{caption}\n{}:{media}",
        js_len(connection_id),
        js_len(caption),
        js_len(media),
    );
    sha256_hex(canonical.as_bytes())
}

fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn valid_key(key: &str) -> bool {
    (8..=128).contains(&key.len())
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn valid_media_key(key: &str) -> bool {
    (1..=MEDIA_CAP).contains(&key.len())
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '~' | '-'))
}

/// `sha256:<hex>` or bare hex. Anything else is a malformed pin.
fn normalize_hash(raw: &str) -> Option<String> {
    let hex = raw.strip_prefix("sha256:").unwrap_or(raw);
    if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(hex.to_ascii_lowercase())
    } else {
        None
    }
}

struct PublishInput {
    connection_id: String,
    caption: String,
    media_key: Option<String>,
}

fn parse_publish(input: &Value) -> std::result::Result<PublishInput, String> {
    let connection_id = input
        .get("connectionId")
        .and_then(Value::as_str)
        .ok_or("publish needs a string connectionId")?
        .to_string();
    if connection_id.is_empty() || js_len(&connection_id) > CONNECTION_CAP {
        return Err("connectionId must be 1-80 characters".into());
    }
    let caption = input
        .get("caption")
        .and_then(Value::as_str)
        .ok_or("publish needs a string caption")?
        .to_string();
    if caption.is_empty() || js_len(&caption) > CAPTION_CAP {
        return Err("caption must be 1-8000 characters".into());
    }
    let media_key = match input.get("mediaKey") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if valid_media_key(s) => Some(s.clone()),
        Some(_) => {
            return Err(
                "mediaKey must be 1-200 characters of letters, digits, '.', '_', '~' or '-'".into(),
            )
        }
    };
    Ok(PublishInput {
        connection_id,
        caption,
        media_key,
    })
}

fn scrub(secret: &[u8], text: String) -> String {
    let secret = String::from_utf8_lossy(secret);
    if secret.is_empty() || !text.contains(secret.as_ref()) {
        text
    } else {
        text.replace(secret.as_ref(), "[redacted]")
    }
}

fn bearer(credential: &[u8]) -> std::result::Result<Option<String>, String> {
    if credential.is_empty() {
        return Ok(None);
    }
    let token = std::str::from_utf8(credential)
        .map_err(|_| "enrolled credential is not UTF-8".to_string())?;
    if token.is_empty() || token.chars().any(|c| c.is_whitespace() || !c.is_ascii()) {
        return Err("enrolled credential is not a single-line token".into());
    }
    Ok(Some(format!("Bearer {token}")))
}

impl AgenticosAdapter {
    fn remember(&self, digest: &str, key: &str, verified: Verified) {
        self.handoffs.lock().unwrap().insert(
            digest.to_string(),
            Handoff {
                key: key.to_string(),
                digest: digest.to_string(),
                verified,
            },
        );
    }

    fn recall(&self, digest: &str) -> Option<Handoff> {
        self.handoffs.lock().unwrap().get(digest).map(|h| Handoff {
            key: h.key.clone(),
            digest: h.digest.clone(),
            verified: h.verified,
        })
    }

    fn publish(
        &self,
        credential: &[u8],
        input: &Value,
        idempotency_key: &str,
        expected_hash: Option<&str>,
    ) -> std::result::Result<Value, String> {
        if !valid_key(idempotency_key) {
            return Err("idempotency key must be 8-128 letters, digits, '_' or '-'".into());
        }
        let post = parse_publish(input)?;
        let digest = publish_content_digest(
            &post.connection_id,
            &post.caption,
            post.media_key.as_deref(),
        );
        if let Some(expected) = expected_hash {
            let expected = normalize_hash(expected)
                .ok_or_else(|| "expected content hash must be sha256:<64 hex>".to_string())?;
            if expected != digest {
                return Err(
                    "expected content hash does not match this revision — not posted".into(),
                );
            }
        }
        let auth = bearer(credential)?;
        let url = format!("{base}{DOOR}/publish", base = self.base);
        let mut body = json!({
            "connectionId": post.connection_id,
            "caption": post.caption,
        });
        if let Some(media) = &post.media_key {
            body["mediaKey"] = json!(media);
        }
        let mut req = self
            .http
            .post(&url)
            .header("idempotency-key", idempotency_key)
            .header("content-digest", &digest);
        if let Some(auth) = &auth {
            req = req.header("authorization", auth);
        }
        let mut resp = req
            .send_json(&body)
            .map_err(|e| scrub(credential, format!("agenticos publish failed: {e}")))?;
        let bytes = resp
            .body_mut()
            .with_config()
            .limit(BODY_CAP as u64)
            .read_to_vec()
            .map_err(|e| scrub(credential, format!("agenticos publish read failed: {e}")))?;
        let payload: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "agenticos publish response is not JSON".to_string())?;
        if payload.get("ok").and_then(Value::as_bool) == Some(false) {
            let code = payload["error"]["code"].as_str().unwrap_or("error");
            return Err(scrub(
                credential,
                format!("agenticos publish refused: {code}"),
            ));
        }
        let data = payload.get("data").cloned().unwrap_or(payload);
        let view = ledger_view(&data, &digest);
        self.remember(&digest, idempotency_key, view.verified);
        Ok(view.into_result(idempotency_key, &digest))
    }

    /// Re-read a handoff the door remembers. A 404 falls back to the
    /// outcome `execute` stored — the live door has no status route
    /// yet, and a missing route is not a receipt mismatch.
    fn refresh(&self, key: &str, digest: &str) -> Option<Verified> {
        let url = format!("{base}{DOOR}/publish/{key}", base = self.base);
        let mut resp = self.http.get(&url).call().ok()?;
        if resp.status().as_u16() == 404 {
            return None;
        }
        let bytes = resp
            .body_mut()
            .with_config()
            .limit(BODY_CAP as u64)
            .read_to_vec()
            .ok()?;
        let payload: Value = serde_json::from_slice(&bytes).ok()?;
        if payload.get("ok").and_then(Value::as_bool) == Some(false) {
            return None;
        }
        let data = payload.get("data").cloned().unwrap_or(payload);
        Some(ledger_view(&data, digest).verified)
    }
}

struct Ledger {
    name: &'static str,
    detail: String,
    verified: Verified,
    decision: String,
    status: String,
    permalink: Option<String>,
    repeated: bool,
}

impl Ledger {
    fn into_result(self, key: &str, digest: &str) -> Value {
        json!({
            "platform_ref": key,
            "handoff": "draft",
            "ledger": self.name,
            "detail": self.detail,
            "decision": self.decision,
            "status": self.status,
            "permalink": self.permalink,
            "content_hash": format!("sha256:{digest}"),
            "repeated": self.repeated,
            "verified": self.verified,
            "deep_link": ACCOUNT_APP,
        })
    }
}

fn ledger_view(data: &Value, expected_hex: &str) -> Ledger {
    let decision = data["decision"].as_str().unwrap_or("").to_string();
    let status = data["status"].as_str().unwrap_or("").to_string();
    let permalink = data["permalink"].as_str().map(str::to_string);
    let repeated = data["repeated"].as_bool().unwrap_or(false);
    let receipt = data["contentDigest"].as_str();
    let (name, verified, detail) = if decision == "declined" || status == "declined" {
        (
            "refused",
            Verified::False,
            "refused in AgenticOS".to_string(),
        )
    } else if status == "failed" {
        (
            "refused",
            Verified::False,
            "publish failed in AgenticOS".to_string(),
        )
    } else if status == "posted" {
        match receipt {
            Some(got) if got != expected_hex => (
                "published",
                Verified::False,
                "receipt does not match the approved revision".to_string(),
            ),
            _ => (
                "published",
                Verified::True,
                "published in AgenticOS".to_string(),
            ),
        }
    } else {
        (
            "waiting",
            Verified::Unknown,
            "waiting in AgenticOS".to_string(),
        )
    };
    Ledger {
        name,
        detail,
        verified,
        decision,
        status,
        permalink,
        repeated,
    }
}

impl PlatformAdapter for AgenticosAdapter {
    fn table(&self) -> &ToolTable {
        &self.table
    }

    fn reported_manifest_version(&self) -> Option<String> {
        Some(MANIFEST_VERSION.to_string())
    }

    fn preview(&self, account: &str, tool: &str, input: &Value) -> String {
        if tool != "publish_post" {
            let label = self
                .table
                .declared(tool)
                .and_then(|d| d.label.clone())
                .unwrap_or_else(|| format!("agenticos/{tool}"));
            return format!("{label} (account {account})");
        }
        match parse_publish(input) {
            Ok(post) => {
                let excerpt: String = post.caption.chars().take(160).collect();
                format!(
                    "Hand off to AgenticOS for approval (account {account}, connection {}). \
                     AgenticOS owns the publish.\n\n{excerpt}",
                    post.connection_id
                )
            }
            Err(e) => format!("publish_post (input will fail at execute): {e}"),
        }
    }

    fn execute(
        &self,
        credential: &[u8],
        tool: &str,
        input: &Value,
        idempotency_key: &str,
        expected_hash: Option<&str>,
    ) -> std::result::Result<Value, String> {
        match tool {
            "publish_post" => self.publish(credential, input, idempotency_key, expected_hash),
            "post_draft" => self.draft(credential, input),
            "connections_list" => self.get(credential, &format!("{DOOR}/connections"), input),
            "connection_profile" => self.connection_view(credential, input, "profile"),
            "connection_posts" => self.connection_view(credential, input, "posts"),
            "connection_insights" => self.connection_view(credential, input, "insights"),
            other => Err(format!(
                "agenticos has no tool '{other}' — the table declares reads, post_draft and publish_post"
            )),
        }
    }

    fn read_back(&self, tool: &str, input: &Value) -> Verified {
        if tool != "publish_post" {
            return Verified::Unknown;
        }
        let Ok(post) = parse_publish(input) else {
            return Verified::Unknown;
        };
        let digest = publish_content_digest(
            &post.connection_id,
            &post.caption,
            post.media_key.as_deref(),
        );
        let Some(saved) = self.recall(&digest) else {
            return Verified::Unknown;
        };
        self.refresh(&saved.key, &digest).unwrap_or(saved.verified)
    }

    fn source_hash(&self, _agent: &str, _source: &str) -> Option<String> {
        None
    }
}

impl AgenticosAdapter {
    fn draft(&self, credential: &[u8], input: &Value) -> std::result::Result<Value, String> {
        let post = parse_publish(input)?;
        let url = format!("{base}{DOOR}/draft", base = self.base);
        let mut body = json!({
            "connectionId": post.connection_id,
            "caption": post.caption,
        });
        if let Some(media) = &post.media_key {
            body["mediaKey"] = json!(media);
        }
        let data = self.round_trip(credential, self.http.post(&url), &body)?;
        Ok(json!({
            "handoff": "draft",
            "platform_ref": data["connectionId"].as_str().unwrap_or(""),
            "preview": data,
        }))
    }

    fn connection_view(
        &self,
        credential: &[u8],
        input: &Value,
        view: &str,
    ) -> std::result::Result<Value, String> {
        let id = input
            .get("connectionId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && js_len(s) <= CONNECTION_CAP)
            .ok_or("connection view needs a connectionId")?;
        if !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err("connectionId contains characters that cannot go in a path".into());
        }
        self.get(
            credential,
            &format!("{DOOR}/connections/{id}/{view}"),
            input,
        )
    }

    fn get(
        &self,
        credential: &[u8],
        path: &str,
        _input: &Value,
    ) -> std::result::Result<Value, String> {
        let url = format!("{base}{path}", base = self.base);
        let mut req = self.http.get(&url);
        if let Some(auth) = bearer(credential)? {
            req = req.header("authorization", &auth);
        }
        let mut resp = req
            .call()
            .map_err(|e| scrub(credential, format!("agenticos read failed: {e}")))?;
        self.decode(credential, &mut resp, "read")
    }

    fn round_trip(
        &self,
        credential: &[u8],
        mut req: ureq::RequestBuilder<ureq::typestate::WithBody>,
        body: &Value,
    ) -> std::result::Result<Value, String> {
        if let Some(auth) = bearer(credential)? {
            req = req.header("authorization", &auth);
        }
        let mut resp = req
            .send_json(body)
            .map_err(|e| scrub(credential, format!("agenticos draft failed: {e}")))?;
        self.decode(credential, &mut resp, "draft")
    }

    fn decode(
        &self,
        credential: &[u8],
        resp: &mut ureq::http::Response<ureq::Body>,
        what: &str,
    ) -> std::result::Result<Value, String> {
        let code = resp.status().as_u16();
        let bytes = resp
            .body_mut()
            .with_config()
            .limit(BODY_CAP as u64)
            .read_to_vec()
            .map_err(|e| scrub(credential, format!("agenticos {what} read failed: {e}")))?;
        let payload: Value = serde_json::from_slice(&bytes)
            .map_err(|_| format!("agenticos {what} response is not JSON"))?;
        if payload.get("ok").and_then(Value::as_bool) == Some(false) || !(200..300).contains(&code)
        {
            let err = payload["error"]["code"].as_str().unwrap_or("error");
            return Err(scrub(
                credential,
                format!("agenticos {what} refused: {err}"),
            ));
        }
        Ok(payload.get("data").cloned().unwrap_or(payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_matches_the_agenticos_length_prefixed_vector() {
        // Independent vector: sha256("6:conn_1\n5:hello\n0:").
        assert_eq!(
            publish_content_digest("conn_1", "hello", None),
            "a33e9c2b6f5a46089ac180273c6365d6e6eb4b2b7447b7d03d0ea92aeb2290b4"
        );
    }

    #[test]
    fn resolve_base_prefers_an_explicit_url_and_defaults_hosted() {
        assert_eq!(
            resolve_base(false, Some(" http://127.0.0.1:9/ ")).as_deref(),
            Some("http://127.0.0.1:9")
        );
        assert_eq!(
            resolve_base(true, None).as_deref(),
            Some("http://api.internal")
        );
        assert_eq!(resolve_base(false, None), None);
        assert_eq!(resolve_base(false, Some("  ")), None);
    }

    #[test]
    fn publish_post_is_a_draft_handoff() {
        let adapter = AgenticosAdapter::new("http://127.0.0.1:9").unwrap();
        assert_eq!(
            adapter.table().effect_of("publish_post"),
            crate::contract_fixture::Effect::Draft
        );
        assert!(adapter
            .table()
            .manifest_matches(adapter.reported_manifest_version().as_deref()));
    }
}
