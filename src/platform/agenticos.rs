//! AgenticOS provider implementation of the generic PlatformAdapter interface.
//! Reviewed against AOS-57 publish@2: authorize attests the canonical ledger
//! key and exact content digest before publish. Pending/declined never publish.
//! Every retry repeats supported authenticated POSTs; no cache or status GET.
//! The upstream durable ledger owns approval, deduplication and restart recovery.
//! Provider-reported posting is not provider-content verification: receipts and
//! credentialless read_back remain Unknown. Identical content intentionally
//! deduplicates; distinct intentions with identical bytes cannot be expressed.
//! Hosted identity comes from the upstream company/instance/lease binding.
//! Explicit local URLs and bearer transport do not establish hosted identity
//! or an operational self-hosted token contract. Media import and other
//! providers are not exposed through arbitrary tool/URL passthrough.
//! Default hosted attach has no external deployment assertion and retains
//! Cadence's Send approval gate. Trusted embedding registration may supply
//! agenticos-manifest@1/publish_post@2 to enable reviewed read/draft effects;
//! this is an operator deployment assertion, never live remote verification.
mod wire;

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

pub const BUILTIN_ACCOUNT: super::BuiltinAccount = super::BuiltinAccount {
    platform: PLATFORM,
    account: HOSTED_ACCOUNT,
};

/// Base URL inside a hosted container. `CADENCE_AGENTICOS_URL`
/// overrides it; a daemon that is not hosted does not register this
/// adapter at all.
pub const HOSTED_BASE: &str = "http://api.internal";

/// Where an operator opens the company that is waiting on the publish.
/// The runtime door has no per-send URL; this is the account app the
/// contract names.
const ACCOUNT_APP: &str = "https://app-v2.agenticos.hk/account";

const DOOR: &str = "/v1/runtime/connectors";

/// Reviewed composite identity: upstream manifest version 1 and publish_post@2.
/// The tool table is review metadata, not a report from a deployed platform.
const TABLE_JSON: &str = r#"{
    "platform": "agenticos",
    "manifest_version": "agenticos-manifest@1/publish_post@2",
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

/// The AgenticOS adapter. `base` is the runtime origin (`http://api.internal`
/// when hosted). Custody bytes, when present, are a scoped bearer token;
/// empty bytes are the hosted path.
pub struct AgenticosAdapter {
    table: ToolTable,
    base: String,
    deployment_pin: Option<String>,
    http: ureq::Agent,
}

impl AgenticosAdapter {
    /// `base` is `http://` or `https://` with a host and no userinfo.
    /// A trailing slash is dropped.
    pub fn new(base: &str) -> Result<Self> {
        Self::with_deployment_pin(base, None)
    }

    /// Trusted embedding composition's assertion about the deployed contract.
    /// This is not live discovery. The composition owner must refresh it when
    /// deployment changes. No URL/hosted identity/tool input infers this value.
    /// Absent or mismatched metadata keeps the generic Cadence Send gate.
    pub fn with_deployment_pin(base: &str, deployment_pin: Option<&str>) -> Result<Self> {
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
            deployment_pin: deployment_pin.map(str::to_owned),
            http: ureq::Agent::new_with_config(config),
        })
    }
}

/// Register the adapter on a daemon that already decided the base URL.
pub fn register(opts: &mut crate::daemon::ServeOptions, base: &str) -> Result<()> {
    register_with_deployment_pin(opts, base, None)
}

/// Explicit trusted provider composition metadata; never an app/worker input.
/// Default registration and hosted attach supply no assertion and stay gated.
pub fn register_with_deployment_pin(
    opts: &mut crate::daemon::ServeOptions,
    base: &str,
    deployment_pin: Option<&str>,
) -> Result<()> {
    let adapter = AgenticosAdapter::with_deployment_pin(base, deployment_pin)?;
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
    // CAD-1063: only a real hosted lease replaces SMTP egress with the
    // platform email door; an explicit URL alone never does.
    if lease_is_on(hosted) && opts.hosted_email.is_none() {
        opts.hosted_email = super::hosted_email::HostedEmail::from_env(&base)?;
    }
    // CAD-1126: the same real lease routes enrolled SMTP senders
    // through the `smtp.internal` pass-through (no direct socket).
    if lease_is_on(hosted) && opts.smtp_internal.is_none() {
        opts.smtp_internal = Some(super::smtp_internal::SmtpInternal::from_env()?);
    }
    register_from_composition(opts, &base, lease_is_on(hosted))
}

/// Trusted embedding route shared by hosted attach. A company lease alone
/// supplies no pin; only exact image metadata can assert this deployment.
pub fn register_from_composition(
    opts: &mut crate::daemon::ServeOptions,
    base: &str,
    hosted: bool,
) -> Result<()> {
    register_with_loader(opts, base, hosted, super::deployments::load)
}

pub(super) fn register_with_loader(
    opts: &mut crate::daemon::ServeOptions,
    base: &str,
    hosted: bool,
    load: impl FnOnce() -> Result<Option<super::deployments::DeploymentMetadata>>,
) -> Result<()> {
    if !hosted {
        return register(opts, base);
    }
    let metadata = match &opts.provider_deployments {
        Some(metadata) => Some(metadata.clone()),
        None => load()?,
    };
    let pin = metadata
        .as_ref()
        .and_then(|metadata| metadata.pin(PLATFORM, base));
    register_with_deployment_pin(opts, base, pin)
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

/// The CompanyControl send key for one revision. The draft gate's
/// `call-<uuid>` is not used: two identical `platform_call`s must hit
/// one row. The alphabet is the door's `^[A-Za-z0-9_-]{8,128}$` — a
/// colon is rejected. Prefix plus a 64-hex digest is 86 characters.
pub fn publish_idempotency_key(digest_hex: &str) -> String {
    format!("agenticos-publish-v1-{digest_hex}")
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
    arguments(input, &["connectionId", "caption", "mediaKey"])?;
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
        None => None,
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

fn typed<T: serde::de::DeserializeOwned>(data: Value) -> std::result::Result<T, String> {
    serde_json::from_value(data)
        .map_err(|_| "agenticos response does not match the reviewed contract".into())
}
fn arguments(input: &Value, allowed: &[&str]) -> std::result::Result<(), String> {
    let object = input.as_object().ok_or("tool input must be an object")?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("unsupported tool argument: only reviewed fields are accepted".into());
    }
    Ok(())
}
fn safe_https(url: &str) -> bool {
    url.parse::<ureq::http::Uri>().is_ok_and(|uri| {
        uri.scheme_str() == Some("https")
            && uri.authority().is_some_and(|a| !a.as_str().contains('@'))
    })
}
fn mirrored(
    key: &str,
    digest: &str,
    decision: wire::Decision,
    status: wire::Status,
    permalink: Option<String>,
    repeated: bool,
) -> Value {
    let (ledger, detail, verified) = match status {
        wire::Status::Declined => ("refused", "refused in AgenticOS", Verified::False),
        wire::Status::Failed => ("refused", "publish failed in AgenticOS", Verified::False),
        wire::Status::Posted => (
            "published",
            "AgenticOS reports posted; provider content is not verified",
            Verified::Unknown,
        ),
        _ => ("waiting", "waiting in AgenticOS", Verified::Unknown),
    };
    json!({"platform_ref":key,"handoff":"draft","ledger":ledger,"detail":detail,"decision":decision,"status":status,"permalink":permalink,"content_hash":format!("sha256:{digest}"),"repeated":repeated,"verified":verified,"deep_link":ACCOUNT_APP})
}
impl AgenticosAdapter {
    fn publish(
        &self,
        credential: &[u8],
        input: &Value,
        _call_key: &str,
        expected_hash: Option<&str>,
    ) -> std::result::Result<Value, String> {
        let post = parse_publish(input)?;
        let digest = publish_content_digest(
            &post.connection_id,
            &post.caption,
            post.media_key.as_deref(),
        );
        if expected_hash.is_some_and(|raw| normalize_hash(raw).as_deref() != Some(digest.as_str()))
        {
            return Err("expected content hash does not match this revision — not posted".into());
        }
        let request_key = publish_idempotency_key(&digest);
        let mut body = json!({"connectionId":post.connection_id,"caption":post.caption});
        if let Some(media) = post.media_key {
            body["mediaKey"] = json!(media);
        }
        let (code, data) = self.publish_request(
            credential,
            "publish/authorize",
            &request_key,
            &digest,
            &body,
        )?;
        if code != 200 {
            return Err("agenticos authorization HTTP status is not successful".into());
        }
        let authorization: wire::Authorization = typed(data)?;
        if !valid_key(&authorization.key)
            || authorization.content_digest.as_deref() != Some(digest.as_str())
        {
            return Err(
                "agenticos authorization key or content digest does not match the approved input"
                    .into(),
            );
        }
        match authorization.decision {
            wire::Decision::Pending => {
                return Ok(mirrored(
                    &authorization.key,
                    &digest,
                    authorization.decision,
                    wire::Status::Pending,
                    None,
                    authorization.repeated,
                ))
            }
            wire::Decision::Declined => {
                return Ok(mirrored(
                    &authorization.key,
                    &digest,
                    authorization.decision,
                    wire::Status::Declined,
                    None,
                    authorization.repeated,
                ))
            }
            _ => {}
        }
        let (code, data) =
            self.publish_request(credential, "publish", &authorization.key, &digest, &body)?;
        let result: wire::Published = typed(data)?;
        let valid = matches!(
            (code, result.decision, result.status, result.executed),
            (
                200,
                wire::Decision::Approved | wire::Decision::Granted,
                wire::Status::Posted | wire::Status::Processing,
                true
            ) | (
                200,
                wire::Decision::Approved | wire::Decision::Granted,
                wire::Status::Failed,
                false
            ) | (202, wire::Decision::Pending, wire::Status::Pending, false)
                | (409, wire::Decision::Declined, wire::Status::Declined, false)
        );
        if !valid
            || result.key != authorization.key
            || result
                .permalink
                .as_deref()
                .is_some_and(|url| !safe_https(url))
        {
            return Err(
                "agenticos execution HTTP status, state or canonical key is inconsistent".into(),
            );
        }
        Ok(mirrored(
            &result.key,
            &digest,
            result.decision,
            result.status,
            result.permalink,
            result.repeated,
        ))
    }
    fn publish_request(
        &self,
        credential: &[u8],
        path: &str,
        key: &str,
        digest: &str,
        body: &Value,
    ) -> std::result::Result<(u16, Value), String> {
        let url = format!("{}{DOOR}/{path}", self.base);
        let mut request = self
            .http
            .post(&url)
            .header("idempotency-key", key)
            .header("content-digest", digest);
        if let Some(auth) = bearer(credential)? {
            request = request.header("authorization", &auth);
        }
        let mut response = request
            .send_json(body)
            .map_err(|e| scrub(credential, format!("agenticos handoff failed: {e}")))?;
        self.response(credential, &mut response, "handoff")
    }
}

impl PlatformAdapter for AgenticosAdapter {
    fn connection_descriptor(&self) -> Option<crate::platform::connections::ProviderDescriptor> {
        use crate::platform::connections::{CapabilityDescriptor, ProviderDescriptor};
        Some(ProviderDescriptor {
            action_mappings: vec![],
            schema: 1,
            provider: "agenticos".into(),
            revision: "agenticos-connections/1".into(),
            enrollment_shapes: vec![],
            builtin_accounts: vec!["hosted".into()],
            capabilities: vec![
                CapabilityDescriptor {
                    id: "sources.discover".into(),
                    version: 1,
                    tools: vec!["connections_list".into()],
                    scopes: vec!["sources".into()],
                    effect: "read".into(),
                    semantics: crate::platform::connections::CapabilitySemantics::MetadataRead,
                },
                CapabilityDescriptor {
                    id: "sources.profile".into(),
                    version: 1,
                    tools: vec!["connection_profile".into()],
                    scopes: vec!["sources".into()],
                    effect: "read".into(),
                    semantics: crate::platform::connections::CapabilitySemantics::MetadataRead,
                },
                CapabilityDescriptor {
                    id: "sources.posts.text".into(),
                    version: 1,
                    tools: vec!["connection_posts".into()],
                    scopes: vec!["sources".into()],
                    effect: "read".into(),
                    semantics: crate::platform::connections::CapabilitySemantics::MetadataRead,
                },
                CapabilityDescriptor {
                    id: "sources.insights".into(),
                    version: 1,
                    tools: vec!["connection_insights".into()],
                    scopes: vec!["sources".into()],
                    effect: "read".into(),
                    semantics: crate::platform::connections::CapabilitySemantics::MetadataRead,
                },
                CapabilityDescriptor {
                    id: "content.preview".into(),
                    version: 1,
                    tools: vec!["post_draft".into()],
                    scopes: vec!["draft".into()],
                    effect: "draft".into(),
                    semantics: crate::platform::connections::CapabilitySemantics::PreviewOnly,
                },
                CapabilityDescriptor {
                    id: "social.post".into(),
                    version: 1,
                    tools: vec!["publish_post".into()],
                    scopes: vec!["publish".into()],
                    effect: "draft".into(),
                    semantics:
                        crate::platform::connections::CapabilitySemantics::UpstreamApprovalHandoff,
                },
            ],
        })
    }
    fn connection_registration(&self) -> Option<String> {
        Some(crate::platform::connections::registration_digest(&format!(
            "agenticos-registration:{}:{:?}",
            self.base, self.deployment_pin
        )))
    }
    fn table(&self) -> &ToolTable {
        &self.table
    }

    fn reported_manifest_version(&self) -> Option<String> {
        self.deployment_pin.clone()
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

    fn read_back(&self, _tool: &str, _input: &Value) -> Verified {
        // This interface carries no current credential. There is no supported
        // authenticated status/read-back contract; never return a cached receipt.
        Verified::Unknown
    }

    fn source_hash(&self, _agent: &str, _source: &str) -> Option<String> {
        None
    }
}

impl AgenticosAdapter {
    fn draft(&self, credential: &[u8], input: &Value) -> std::result::Result<Value, String> {
        let post = parse_publish(input)?;
        let mut body = json!({"connectionId":post.connection_id,"caption":post.caption});
        if let Some(media) = post.media_key {
            body["mediaKey"] = json!(media);
        }
        let url = format!("{}{DOOR}/draft", self.base);
        let mut request = self.http.post(&url);
        if let Some(auth) = bearer(credential)? {
            request = request.header("authorization", &auth);
        }
        let mut response = request
            .send_json(&body)
            .map_err(|e| scrub(credential, format!("agenticos draft failed: {e}")))?;
        let (code, data) = self.response(credential, &mut response, "draft")?;
        let draft: wire::Draft = typed(data)?;
        if code != 200
            || draft.connection_id != post.connection_id
            || draft.caption != post.caption
            || draft.display_name.is_empty()
            || draft
                .media_url
                .as_deref()
                .is_some_and(|url| !safe_https(url))
        {
            return Err("agenticos draft response is inconsistent with the request".into());
        }
        Ok(json!({"handoff":"draft","platform_ref":draft.connection_id,"preview":draft}))
    }
    fn connection_view(
        &self,
        credential: &[u8],
        input: &Value,
        view: &str,
    ) -> std::result::Result<Value, String> {
        arguments(input, &["connectionId"])?;
        let id = input["connectionId"]
            .as_str()
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= CONNECTION_CAP
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            })
            .ok_or("connection view needs a valid connectionId")?;
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
        input: &Value,
    ) -> std::result::Result<Value, String> {
        let list = path == format!("{DOOR}/connections");
        let url = format!("{}{path}", self.base);
        let mut request = self.http.get(&url);
        if list {
            arguments(input, &["cursor", "limit"])?;
            if let Some(cursor) = input.get("cursor") {
                let cursor = cursor
                    .as_str()
                    .filter(|v| !v.is_empty() && v.len() <= 1024)
                    .ok_or("cursor must be a nonempty bounded string")?;
                request = request.query("cursor", cursor);
            }
            if let Some(limit) = input.get("limit") {
                let limit = limit
                    .as_u64()
                    .filter(|v| (1..=100).contains(v))
                    .ok_or("limit must be an integer from 1 to 100")?;
                request = request.query("limit", limit.to_string());
            }
        }
        if let Some(auth) = bearer(credential)? {
            request = request.header("authorization", &auth);
        }
        let mut response = request
            .call()
            .map_err(|e| scrub(credential, format!("agenticos read failed: {e}")))?;
        let (code, data) = self.response(credential, &mut response, "read")?;
        if code != 200 {
            return Err("agenticos read HTTP status is not successful".into());
        }
        if list {
            let page: wire::Connections = typed(data.clone())?;
            if page.connections.len() > 100
                || page.connections.iter().any(|c| {
                    c.id.is_empty()
                        || c.display_name.is_empty()
                        || c.created_at.is_empty()
                        || c.updated_at.is_empty()
                })
                || page
                    .cursor
                    .as_deref()
                    .is_some_and(|v| v.is_empty() || v.len() > 1024)
            {
                return Err("agenticos connection page is malformed".into());
            }
        } else if path.ends_with("/profile") {
            let profile: wire::Profile = typed(data.clone())?;
            if profile
                .accounts
                .iter()
                .any(|a| a.external_id.is_empty() || a.name.is_empty())
            {
                return Err("agenticos profile is malformed".into());
            }
        } else if path.ends_with("/posts") {
            let posts: wire::Posts = typed(data.clone())?;
            if posts.posts.iter().any(|p| {
                p.id.is_empty() || p.permalink.as_deref().is_some_and(|url| !safe_https(url))
            }) {
                return Err("agenticos post metadata is malformed".into());
            }
        } else {
            let insights: wire::Insights = typed(data.clone())?;
            if insights.insights.iter().any(|i| i.name.is_empty()) {
                return Err("agenticos insights are malformed".into());
            }
        }
        // Quoted provider text remains untrusted metadata, never instructions.
        Ok(data)
    }
    fn response(
        &self,
        credential: &[u8],
        response: &mut ureq::http::Response<ureq::Body>,
        what: &str,
    ) -> std::result::Result<(u16, Value), String> {
        let code = response.status().as_u16();
        let bytes = response
            .body_mut()
            .with_config()
            .limit(BODY_CAP as u64)
            .read_to_vec()
            .map_err(|e| scrub(credential, format!("agenticos {what} read failed: {e}")))?;
        let payload: Value = serde_json::from_slice(&bytes)
            .map_err(|_| format!("agenticos {what} response is not JSON"))?;
        let object = payload
            .as_object()
            .ok_or("agenticos response envelope is malformed")?;
        if object.len() != 2 {
            return Err("agenticos response envelope is malformed".into());
        }
        if object.get("ok").and_then(Value::as_bool) == Some(false) {
            let error = payload["error"]["code"]
                .as_str()
                .filter(|code| {
                    code.len() <= 80 && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                })
                .unwrap_or("error");
            return Err(scrub(
                credential,
                format!("agenticos {what} refused: {error}"),
            ));
        }
        if object.get("ok").and_then(Value::as_bool) != Some(true)
            || !object.get("data").is_some_and(Value::is_object)
        {
            return Err("agenticos response envelope is malformed".into());
        }
        Ok((code, payload["data"].clone()))
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
    fn publish_post_handoff_requires_a_trusted_deployment_assertion() {
        use crate::contract_fixture::{classify_call, Effect};
        let unpinned = AgenticosAdapter::new("http://127.0.0.1:9").unwrap();
        assert_eq!(unpinned.reported_manifest_version(), None);
        assert_eq!(
            classify_call(unpinned.table(), None, "publish_post"),
            Effect::Send
        );
        let adapter = AgenticosAdapter::with_deployment_pin(
            "http://127.0.0.1:9",
            Some("agenticos-manifest@1/publish_post@2"),
        )
        .unwrap();
        assert_eq!(
            adapter.table().effect_of("publish_post"),
            crate::contract_fixture::Effect::Draft
        );
        assert!(adapter
            .table()
            .manifest_matches(adapter.reported_manifest_version().as_deref()));
        assert_eq!(
            classify_call(
                adapter.table(),
                adapter.reported_manifest_version().as_deref(),
                "publish_post"
            ),
            Effect::Draft
        );
    }
}
