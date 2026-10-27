//! CAD-1085: the trusted native credential-entry routes.
//!
//! These are the only HTTP surfaces that can carry a credential value, and
//! they exist so a *human* can put one into protected custody:
//!
//! - `GET  /connections/enroll/<request_id>` — the client-side entry page
//!   (the SPA shell). It carries no credential and no authority. The server
//!   answers it ONLY on the root-pinned entry origin; the ordinary board
//!   404s it, so the board SPA can never mount the form.
//! - `GET  /api/credential-requests/<id>` — metadata only; board or entry
//!   origin with a live session.
//! - `GET  /api/credential-requests?conversation=<id>` — metadata only.
//! - `POST /api/credential-requests` — create a metadata request (the
//!   Settings/agent-detail protected path; the CLI uses the daemon RPC).
//!   Metadata only, so the ordinary board origin may create it.
//! - `POST /api/credential-requests/<id>/session` — establish the ENTRY
//!   session proof and receive ONE one-use form nonce. Entry origin only.
//! - `POST /api/credential-requests/<id>/submit` — claim the nonce and hand
//!   the bounded secret to the daemon's provider mutation once. Entry origin
//!   only.
//! - `POST /api/credential-requests/<id>/cancel` — close an unclaimed
//!   request. Entry origin only; an attempt already claimed may be in
//!   flight, and cancel never promises to undo it.
//!
//! [`gate`] runs in the dispatcher before any handler or body read. It
//! refuses the secret-bearing routes and the entry page on every Host but
//! the exact pinned origin (closed `wrong_entry_origin`, never a body), and
//! on the pinned origin it admits only that page, the sign-in/shell, and
//! the credential-request API. On a protected profile it also refuses the
//! legacy directly-typed connection bodies. The accepted write then passes
//! the normal write guard and the daemon re-proves the human; the form nonce
//! is bound to the daemon-issued ENTRY session (a SHA-256 digest of its id
//! and pinned origin — the raw token never leaves the cookie).
//!
//! Error bodies are closed: a fixed code plus a fixed message derived only
//! from that code. No daemon text, host name, account or provider
//! diagnostic is ever reflected, and every response is `no-store`.

use super::{err_response, header_value, json_response, read_body, HttpResp, ServeOpts};
use crate::client;
use crate::error::Error;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use tiny_http::{Header, Method, Request, Response, StatusCode};

/// Management JSON bound (r3 §F3); the submit path is narrower.
const BODY_CAP: u64 = 256 * 1024;
const SUBMIT_CAP: u64 = 96 * 1024;
const RESULT_CAP: usize = 256 * 1024;
/// The fixed auth-only cross-origin endpoint on the pinned entry origin.
pub(super) const AUTH_PATH: &str = "/api/credential-entry/authenticate";
/// The board-origin metadata endpoint that mints one purpose-bound nonce.
pub(super) const PREPARE_PATH: &str = "/api/credential-entry/prepare";
const AUTH_CAP: u64 = 8 * 1024;

/// A credential form exists only on the deployment-pinned origin. Every
/// other Host — the ordinary board, the tailnet name, the public board,
/// a hand-set header — is refused BEFORE the body is read.
pub(super) const ENTRY_ORIGIN_REQUIRED: &str = "Credential entry is served only on the configured trusted entry origin. Nothing was collected.";
/// The legacy directly-typed connection forms are unsupported on a
/// protected profile; the board refuses them before reading a body.
pub(super) const PROTECTED_ORIGIN_REQUIRED: &str = "This protected profile collects credentials only on the trusted entry origin. Nothing was collected.";

/// Does this Host name the deployment-pinned credential-entry origin?
/// Used only by the dispatcher's Host allowlist; ordinary board routes are
/// still refused there by [`gate`].
pub(super) fn accepts_host(opts: &ServeOpts, host: &str) -> bool {
    EntryOrigin::from_opts(opts).is_some_and(|entry| entry.matches_host(host))
}

/// The exact deployment-pinned credential-entry origin: `https://host[:port]`
/// from the root profile, never a request field, `ui.json`, CLI flag or
/// environment value. [`EntryOrigin::from_opts`] additionally refuses an
/// origin that is also a board origin, so "separate" is structural rather
/// than a copy claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct EntryOrigin {
    origin: String,
    host: String,
}

impl EntryOrigin {
    fn parse(raw: &str) -> Option<Self> {
        let rest = raw.strip_prefix("https://")?;
        if rest.is_empty()
            || rest.len() > 200
            || rest
                .bytes()
                .any(|b| b.is_ascii_whitespace() || matches!(b, b'/' | b'?' | b'#' | b'@'))
            || rest.contains("..")
        {
            return None;
        }
        let host = rest.trim_end_matches(":443").to_ascii_lowercase();
        if host.is_empty() || host.starts_with(':') || host.ends_with(':') {
            return None;
        }
        Some(Self {
            origin: format!("https://{rest}"),
            host,
        })
    }

    /// The validated entry origin for these serve options. `None` means the
    /// deployment has no separate trusted origin (explicitly unsupported:
    /// no credential form is served anywhere) or the profile named one that
    /// is also a board origin.
    pub(super) fn from_opts(opts: &ServeOpts) -> Option<Self> {
        let entry = Self::parse(opts.credential_entry_origin.as_deref()?)?;
        entry.separate_from_board(opts).then_some(entry)
    }

    fn separate_from_board(&self, opts: &ServeOpts) -> bool {
        let host = &self.host;
        let own = super::operator::board_host(opts.port);
        let own_name = own.rsplit_once(':').map(|(name, _)| name).unwrap_or(&own);
        if host_eq(host, &own) || host_eq(host, own_name) {
            return false;
        }
        if host_eq(host, &format!("{}:{}", opts.host, opts.port)) {
            return false;
        }
        if opts
            .public
            .as_ref()
            .is_some_and(|public| host_eq(host, &public.host))
        {
            return false;
        }
        if let Some((dns, port)) = &opts.tailnet {
            if host_eq(host, dns) || host_eq(host, &format!("{dns}:{port}")) {
                return false;
            }
        }
        if opts.allow_hosts.iter().any(|other| host_eq(host, other)) {
            return false;
        }
        !matches!(
            normalize_host(host).as_str(),
            "localhost" | "127.0.0.1" | "::1" | "[::1]"
        )
    }

    /// The `Host` forms a browser may send for this origin: the bare host
    /// (default port omitted) and, defensively, the explicit `:443` form an
    /// HTTPS proxy may forward.
    pub(super) fn host_forms(&self) -> Vec<String> {
        if self.host.contains(':') {
            vec![self.host.clone()]
        } else {
            vec![self.host.clone(), format!("{}:443", self.host)]
        }
    }

    pub(super) fn origin(&self) -> &str {
        &self.origin
    }

    fn url(&self, request_id: &str) -> String {
        format!("{}/connections/enroll/{}", self.origin, request_id)
    }

    pub(super) fn matches_host(&self, raw: &str) -> bool {
        host_eq(&self.host, raw)
    }

    pub(super) fn matches_origin(&self, raw: &str) -> bool {
        let Some(rest) = raw.trim().strip_prefix("https://") else {
            return false;
        };
        !rest.bytes().any(|b| b.is_ascii_whitespace()) && host_eq(&self.host, rest)
    }
}

/// Lowercase, one trailing dot and the default HTTPS port ignored — the
/// comparisons a browser's `Host`/`Origin` and the pinned URL agree on.
fn normalize_host(raw: &str) -> String {
    let host = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    host.strip_suffix(":443").unwrap_or(&host).to_string()
}

fn host_eq(a: &str, b: &str) -> bool {
    normalize_host(a) == normalize_host(b)
}

fn request_host(request: &Request) -> String {
    header_value(request, "Host").unwrap_or_default()
}

/// A credential-bearing route: the one-use session, the single submit and
/// the cancel of an unclaimed request. These exist only on the pinned
/// origin; the ordinary board refuses them before reading a body.
pub(super) fn secret_route(path: &str) -> bool {
    matches!(
        route(path),
        Some(Route::EntrySession(_) | Route::Submit(_) | Route::Cancel(_))
    )
}

/// The top-level trusted entry page (`/connections/enroll/<uuid>`). Its
/// shell is served only on the pinned origin, so the ordinary board SPA
/// can never mount the form.
pub(super) fn entry_page_id(path: &str) -> Option<&str> {
    let id = path.strip_prefix("/connections/enroll/")?;
    let id = id.strip_suffix('/').unwrap_or(id);
    (!id.contains('/') && request_id(id)).then_some(id)
}

pub(super) fn is_entry_page(path: &str) -> bool {
    entry_page_id(path).is_some()
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// The unauthenticated entry URL: a bounded, server-rendered handoff page.
/// It carries no SPA, no key, no storage and no credential input; its only
/// action is the selector-only link to the ordinary Board authorization
/// page. The trusted entry UI itself is mounted only by the auth POST
/// response in the same document.
pub(super) fn handoff_document(opts: &ServeOpts, request_id: &str) -> HttpResp {
    let board = super::operator::board_origin(opts);
    let href = format!(
        "{}/connections/authorize/{}",
        escape_html(&board),
        escape_html(request_id)
    );
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"referrer\" content=\"no-referrer\">\
         <title>Protected credential entry</title><style>\
         body{{margin:0;background:#0b0b0c;color:#e8e8ea;font:15px/1.5 system-ui,sans-serif}}\
         main{{max-width:34rem;margin:12vh auto;padding:0 1.25rem}}\
         h1{{font-size:1.35rem;margin:0 0 .75rem}}\
         p{{margin:.75rem 0;color:#b8b8c0}}\
         a{{display:inline-block;padding:.65rem .9rem;border-radius:.5rem;\
         background:#f4f5f6;color:#0b0b0c;text-decoration:none;font-weight:600}}\
         .hint{{font:12px/1.4 ui-monospace,monospace;color:#77777f;word-break:break-all}}\
         </style></head><body><main>\
         <h1>Protected credential entry</h1>\
         <p>Continue from your Cadence board. The credential is typed only on this \
         trusted origin; this page never asks for it.</p>\
         <p><a href=\"{href}\">Continue from the Cadence board</a></p>\
         <p class=\"hint\">Request {rid}</p></main></body></html>",
        rid = escape_html(request_id),
    );
    document_response(html.into_bytes(), 200)
}

/// The legacy directly-typed credential transports (a token or SMTP secret
/// in the ordinary create/rotate body). A protected profile refuses them
/// before reading a body; the trusted entry page is the only collector.
fn legacy_secret_route(path: &str) -> bool {
    if path == "/api/connections" {
        return true;
    }
    let Some(tail) = path.strip_prefix("/api/connections/") else {
        return false;
    };
    matches!(tail.split_once('/'), Some((id, "rotate")) if !id.is_empty())
}

/// What the pinned origin serves: its entry page, the shell/assets and
/// sign-in it needs, and the credential-request metadata/one-use routes.
/// Everything else — every other board screen and API — is absent there.
fn entry_origin_allows(path: &str) -> bool {
    path.starts_with("/assets/")
        || path == "/login"
        || path == "/login/"
        || is_entry_page(path)
        || matches!(
            path,
            "/api/meta"
                | "/api/health"
                | "/api/version"
                | "/api/locale"
                | "/api/protected-entry"
                | "/api/session"
                | "/api/session/logout"
                | "/api/session/device/code"
                | "/api/session/device/poll"
        )
        || matches!(
            route(path),
            Some(
                Route::Collection
                    | Route::Show(_)
                    | Route::EntrySession(_)
                    | Route::Submit(_)
                    | Route::Cancel(_)
            )
        )
}

/// The single origin gate `handle` runs before any handler or body read.
/// It returns the refusal for this request, or `None` when the request is
/// on the ordinary board path unchanged.
pub(super) fn gate(
    request: &Request,
    opts: &ServeOpts,
    path: &str,
    is_write: bool,
) -> Option<HttpResp> {
    let entry = EntryOrigin::from_opts(opts);
    let at_entry = entry
        .as_ref()
        .is_some_and(|entry| entry.matches_host(&request_host(request)));
    if let Some(entry) = entry.filter(|_| at_entry) {
        if is_write
            && !entry.matches_origin(header_value(request, "Origin").as_deref().unwrap_or(""))
        {
            return Some(wire_refusal(
                403,
                "wrong_entry_origin",
                ENTRY_ORIGIN_REQUIRED,
            ));
        }
        if !entry_origin_allows(path) {
            return Some(err_response(404, "not found"));
        }
        return None;
    }
    if is_entry_page(path) {
        // The form's shell is not served on any other origin.
        return Some(err_response(404, "not found"));
    }
    if secret_route(path) {
        // Refused before the body: this host may not carry a credential.
        return Some(wire_refusal(
            403,
            "wrong_entry_origin",
            ENTRY_ORIGIN_REQUIRED,
        ));
    }
    if is_write
        && (opts.protected_profile || opts.credential_entry_origin.is_some())
        && legacy_secret_route(path)
    {
        return Some(wire_refusal(
            403,
            "protected_origin_required",
            PROTECTED_ORIGIN_REQUIRED,
        ));
    }
    None
}

/// The live entry-origin session proof the board forwards. It is issued by
/// this entry origin only: the board reads the distinct entry cookie and
/// forwards `{token,key,origin:"loopback",entry_origin}`; the daemon
/// re-verifies an entry-scoped session and binds it to the root profile.
/// The browser never supplies this value, and a board-origin session is
/// not accepted as one.
fn entry_origin_proof(request: &Request, opts: &ServeOpts) -> Result<serde_json::Value, HttpResp> {
    super::operator::credential_entry_session_proof(request, opts).ok_or_else(|| {
        wire_refusal(
            401,
            "entry_session_required",
            super::operator::ENTRY_SESSION_REQUIRED,
        )
    })
}

/// The ordinary board-origin operator proof for protected management:
/// management is metadata/authority only, carries no credential body, and
/// stays on the board origin.
fn board_session_proof(request: &Request, opts: &ServeOpts) -> Result<serde_json::Value, HttpResp> {
    super::operator::chat_upload_session_proof(request, opts).ok_or_else(|| {
        wire_refusal(
            401,
            "entry_session_required",
            super::operator::ENTRY_SESSION_REQUIRED,
        )
    })
}

/// Attach `entry_url` to the credential-request rows the board answers.
/// It is the daemon's metadata plus one server-selected absolute URL; no
/// value, fragment or credential is ever added.
pub(super) fn attach_entry_urls(value: &mut Value, opts: &ServeOpts) {
    let Some(entry) = EntryOrigin::from_opts(opts) else {
        return;
    };
    if let Some(requests) = value.get_mut("requests").and_then(Value::as_array_mut) {
        for request in requests {
            attach_entry_url(request, &entry);
        }
    }
    if let Some(request) = value.get_mut("request") {
        attach_entry_url(request, &entry);
    }
}

fn attach_entry_url(value: &mut Value, entry: &EntryOrigin) {
    if let Some(id) = value.get("request_id").and_then(Value::as_str) {
        if request_id(id) {
            value["entry_url"] = json!(entry.url(id));
        }
    }
}

/// `GET /api/protected-entry` — the SPA's origin gate. Contains the pinned
/// origin and the board's own origin only; no request, credential or
/// capability. A profile with no separate origin reports `configured:false`.
pub(super) fn config_json(opts: &ServeOpts) -> Value {
    let entry = EntryOrigin::from_opts(opts);
    let board_origin = if let Some(public) = &opts.public {
        format!(
            "{}://{}",
            super::operator::public_scheme(&public.host),
            public.host
        )
    } else if let Some((dns, port)) = &opts.tailnet {
        super::tailnet_url(dns, *port)
    } else {
        format!("http://{}", super::operator::board_host(opts.port))
    };
    json!({
        "configured": opts.protected_profile || entry.is_some(),
        "protected": opts.protected_profile || entry.is_some(),
        "origin": entry.as_ref().map(EntryOrigin::origin),
        "board_origin": board_origin,
    })
}

#[derive(Clone, Copy)]
pub(super) enum Route<'a> {
    /// `GET` lists, `POST` creates.
    Collection,
    Show(&'a str),
    EntrySession(&'a str),
    Submit(&'a str),
    Cancel(&'a str),
    /// Protected management: a one-use nonce, then the typed action.
    ManagementSession,
    ManagementAction,
    ManagementStatus,
    /// Board-origin human Continue: mint one purpose-bound entry nonce.
    Prepare,
}

impl Route<'_> {
    pub(super) fn is_read(self) -> bool {
        matches!(
            self,
            Self::Collection | Self::Show(_) | Self::ManagementStatus
        )
    }
}

/// Canonical lowercase UUID, the only request-id shape.
fn request_id(value: &str) -> bool {
    value.len() == 36
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase() || b == b'-')
        && uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
}

pub(super) fn route(path: &str) -> Option<Route<'_>> {
    if path == "/api/credential-requests" {
        return Some(Route::Collection);
    }
    if path == "/api/protected-management/session" {
        return Some(Route::ManagementSession);
    }
    if path == "/api/protected-management/action" {
        return Some(Route::ManagementAction);
    }
    if path == "/api/protected-management/status" {
        return Some(Route::ManagementStatus);
    }
    if path == PREPARE_PATH {
        return Some(Route::Prepare);
    }
    let tail = path.strip_prefix("/api/credential-requests/")?;
    if let Some((id, verb)) = tail.split_once('/') {
        if !request_id(id) {
            return None;
        }
        return match verb {
            "session" => Some(Route::EntrySession(id)),
            "submit" => Some(Route::Submit(id)),
            "cancel" => Some(Route::Cancel(id)),
            _ => None,
        };
    }
    request_id(tail).then_some(Route::Show(tail))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SubmitBody {
    nonce: String,
    #[serde(
        default,
        deserialize_with = "present_secret",
        skip_serializing_if = "Option::is_none"
    )]
    secret: Option<String>,
    /// Fresh SMTP enrollment only: the operator's transport/sender values.
    /// They live in this one request, never in the request rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transport: Option<TransportBody>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TransportBody {
    host: String,
    port: u16,
    tls_mode: String,
    username: String,
    sender: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sender_name: Option<String>,
}

/// An absent field is `None`; a present one must be a string — explicit
/// `null`, a number or an object is refused rather than coerced.
fn present_secret<'de, D>(de: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<serde_json::Value>::deserialize(de)? {
        None => Ok(None),
        Some(serde_json::Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(serde::de::Error::custom("the credential must be a string")),
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CreateBody {
    provider: String,
    verb: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    connection_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shape: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scopes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_credential_revision: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_authorization_revision: Option<i64>,
    /// An existing agent alias the request is for; the daemon resolves its
    /// stable UID, never accepts a caller-supplied UID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    /// Server-resolved opaque target for a non-Connection source. The browser
    /// never supplies it for a Connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_id: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ManagementSessionBody {
    action: String,
    #[serde(default)]
    params: serde_json::Value,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ManagementActionBody {
    attempt_id: String,
    nonce: String,
    action: String,
    #[serde(default)]
    params: serde_json::Value,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PrepareBody {
    request_id: String,
}

impl PrepareBody {
    fn valid(&self) -> bool {
        request_id(&self.request_id)
    }
}

fn typed<T: serde::de::DeserializeOwned>(request: &mut Request, cap: u64) -> Result<T, HttpResp> {
    let body = read_body(request, cap)?;
    serde_json::from_slice(&body).map_err(|_| {
        wire_refusal(
            400,
            "invalid_request",
            "This form could not be accepted. Nothing was sent.",
        )
    })
}

fn empty_body(request: &mut Request) -> Result<(), HttpResp> {
    let body = read_body(request, BODY_CAP)?;
    if body.is_empty() {
        return Ok(());
    }
    serde_json::from_slice::<Empty>(&body)
        .map(|_| ())
        .map_err(|_| {
            wire_refusal(
                400,
                "invalid_request",
                "This form could not be accepted. Nothing was sent.",
            )
        })
}

/// One fixed projection per closed code. The daemon's own message is never
/// reflected: it can name an account, host or provider detail.
fn wire_refusal(status: u16, code: &str, message: &str) -> HttpResp {
    let body = serde_json::to_vec_pretty(&json!({
        "ok": false,
        "error": {"v": 1, "code": code, "message": message},
        "code": code,
        "error_message": message,
    }))
    .unwrap_or_default();
    let mut resp = Response::from_data(body).with_status_code(StatusCode(status));
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    resp.add_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
    resp
}

fn operator_refusal(error: &Error) -> bool {
    let text = error.to_string();
    text.contains("operator action") || text.contains("not provably the operator")
}

/// Fixed status/code/message for every daemon refusal this surface can
/// receive. Unknown codes keep a vague 503 rather than inventing one.
fn daemon_refusal(error: &Error) -> HttpResp {
    if operator_refusal(error) {
        return wire_refusal(
            403,
            "management_forbidden",
            "Credential entry is the signed-in operator's action; this request is not.",
        );
    }
    let code = error
        .code()
        .or_else(|| (error.kind() == "unknown").then_some("outcome_unknown"));
    match code {
        Some("unsupported_verification") => wire_refusal(
            400,
            "unsupported_verification",
            "This provider has no reviewed verification adapter. Nothing was checked.",
        ),
        Some("entry_session_required") => wire_refusal(
            401,
            "entry_session_required",
            super::operator::ENTRY_SESSION_REQUIRED,
        ),
        Some("already_submitted") => wire_refusal(
            409,
            "already_submitted",
            "This form was already submitted. It cannot be sent twice.",
        ),
        Some("nonce_expired") => wire_refusal(
            409,
            "nonce_expired",
            "This form expired. Reopen the entry page for a fresh one.",
        ),
        Some("request_changed") => wire_refusal(
            409,
            "request_changed",
            "This request no longer matches the connection or custody state. Start a new request.",
        ),
        Some("request_not_ready") => wire_refusal(
            503,
            "request_not_ready",
            "Protected credential entry is not ready on this daemon. Nothing was collected.",
        ),
        Some("invalid_request") => wire_refusal(
            400,
            "invalid_request",
            "This form could not be accepted. Nothing was sent.",
        ),
        Some("outcome_unknown") => wire_refusal(
            409,
            "outcome_unknown",
            "The outcome is unknown. Do not submit again; reconcile the request's status.",
        ),
        Some("refused") => wire_refusal(
            409,
            "refused",
            "The credential was refused. Nothing more was changed.",
        ),
        _ => wire_refusal(
            503,
            "entry_unavailable",
            "Protected credential entry is unavailable. Nothing was collected.",
        ),
    }
}

fn ok_json(value: serde_json::Value) -> HttpResp {
    let mut resp = json_response(value);
    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    resp.add_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
    resp
}

fn hex64(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn prepare_reply_ok(value: &Value, _request_id: &str, entry: Option<&EntryOrigin>) -> bool {
    // The origin is the server's pinned value; the request id is the one
    // this board page sent. The daemon returns a nonce bound to both.
    entry.is_some() && value["nonce"].as_str().is_some_and(hex64)
}

/// The private entry document: the actual SPA shell with a server-owned
/// private-mode marker (and, after a successful auth POST, the one-use
/// in-memory bootstrap key plus a selector-only `history.replaceState`).
/// A reload loses the key; the cookie alone cannot recover it.
pub(super) struct EntryBootstrap<'a> {
    pub key: &'a str,
    pub request_id: &'a str,
}

pub(super) fn entry_document(
    opts: &ServeOpts,
    bootstrap: Option<EntryBootstrap<'_>>,
) -> Option<Vec<u8>> {
    let (_name, bytes) = super::serve::static_file(opts.dist.as_deref(), "/index.html")?;
    let html = String::from_utf8(bytes).ok()?;
    let mut inject = String::from(
        "<meta name=\"cadence-entry-mode\" content=\"1\"><script>\
         window.__CADENCE_ENTRY_MODE__=true;",
    );
    if let Some(bootstrap) = bootstrap {
        if !hex64(bootstrap.key) || !request_id(bootstrap.request_id) {
            return None;
        }
        let path = format!("/connections/enroll/{}", bootstrap.request_id);
        inject.push_str(&format!(
            "window.__CADENCE_ENTRY_BOOTSTRAP__={{key:{},requestId:{}}};\
             history.replaceState(null,\"\",{});",
            json!(bootstrap.key),
            json!(bootstrap.request_id),
            json!(path),
        ));
    }
    inject.push_str("</script>");
    let at = html.find("<script").or_else(|| html.find("</head>"))?;
    let mut out = String::with_capacity(html.len() + inject.len());
    out.push_str(&html[..at]);
    out.push_str(&inject);
    out.push_str(&html[at..]);
    Some(out.into_bytes())
}

/// Bounded no-store, non-frameable private entry document response.
pub(super) fn document_response(bytes: Vec<u8>, status: u16) -> HttpResp {
    let mut resp = Response::from_data(bytes).with_status_code(StatusCode(status));
    for (name, value) in [
        ("Content-Type", "text/html; charset=utf-8"),
        ("Cache-Control", "no-store"),
        ("X-Content-Type-Options", "nosniff"),
        ("X-Frame-Options", "DENY"),
        ("Referrer-Policy", "no-referrer"),
        (
            "Content-Security-Policy",
            "default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' \
             'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; connect-src 'self'; \
             frame-ancestors 'none'; form-action 'self'; base-uri 'none'",
        ),
    ] {
        if let Ok(header) = Header::from_bytes(name, value) {
            resp.add_header(header);
        }
    }
    resp
}

fn auth_refusal(status: u16) -> HttpResp {
    document_response(
        b"<!doctype html><html><head><meta charset=\"utf-8\"><title>Sign-in</title></head>\
          <body><h1>This sign-in could not be completed.</h1>\
          <p>Return to the Cadence board and start again.</p></body></html>"
            .to_vec(),
        status,
    )
}

struct AuthFields {
    nonce: String,
    request_id: String,
}

fn set_once(slot: &mut Option<String>, value: String) -> Result<(), HttpResp> {
    if slot.is_some() {
        return Err(auth_refusal(400));
    }
    *slot = Some(value);
    Ok(())
}

fn form_value(raw: &str) -> Option<String> {
    super::pct_decode(&raw.replace('+', " "))
}

/// Strict `application/x-www-form-urlencoded` body: exactly nonce and
/// request_id, once each, no unknown/duplicate field. The entry origin is
/// the server's pinned URI, never a form field.
fn auth_form(body: &[u8]) -> Result<AuthFields, HttpResp> {
    let text = std::str::from_utf8(body).map_err(|_| auth_refusal(400))?;
    let mut nonce = None;
    let mut rid_field = None;
    for pair in text.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = form_value(key).ok_or_else(|| auth_refusal(400))?;
        let value = form_value(value).ok_or_else(|| auth_refusal(400))?;
        match key.as_str() {
            "nonce" => set_once(&mut nonce, value)?,
            "request_id" => set_once(&mut rid_field, value)?,
            _ => return Err(auth_refusal(400)),
        }
    }
    let nonce = nonce.ok_or_else(|| auth_refusal(400))?;
    let rid = rid_field.ok_or_else(|| auth_refusal(400))?;
    if !hex64(&nonce) || !request_id(&rid) {
        return Err(auth_refusal(400));
    }
    Ok(AuthFields {
        nonce,
        request_id: rid,
    })
}

/// The ONE auth-only cross-origin exception on the pinned entry origin.
/// Exact path/method, exact configured Board Origin, form content type,
/// strict bounded body, non-agent peer, and a purpose/RID-bound nonce. It
/// never accepts credentials, redirects, a principal or a target origin;
/// the secret routes never use it.
pub(super) fn authenticate(request: &mut Request, state: &Path, opts: &ServeOpts) -> HttpResp {
    let Some(entry) = EntryOrigin::from_opts(opts) else {
        return err_response(404, "not found");
    };
    if !entry.matches_host(&request_host(request)) {
        return err_response(404, "not found");
    }
    if request.method() != &Method::Post {
        return auth_refusal(405);
    }
    if request
        .url()
        .split_once('?')
        .is_some_and(|(_, query)| !query.is_empty())
    {
        return auth_refusal(400);
    }
    if header_value(request, "Content-Type")
        .as_deref()
        .map(str::trim)
        != Some("application/x-www-form-urlencoded")
    {
        return auth_refusal(400);
    }
    let Some(origin) = header_value(request, "Origin") else {
        return auth_refusal(403);
    };
    if !super::operator::is_board_origin(opts, &origin) {
        return auth_refusal(403);
    }
    // The reviewed transport is a TOP-LEVEL form POST. When the browser
    // states its fetch metadata, require that shape (older clients omit
    // it); this is not a fetch/XHR endpoint.
    if let Some(mode) = header_value(request, "Sec-Fetch-Mode") {
        if !mode.trim().eq_ignore_ascii_case("navigate") {
            return auth_refusal(403);
        }
    }
    if let Err(response) = super::operator::authenticate_peer(request, state, opts) {
        return response;
    }
    let body = match read_body(request, AUTH_CAP) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let fields = match auth_form(&body) {
        Ok(fields) => fields,
        Err(response) => return response,
    };
    let value = match client::rpc(
        state,
        "credential_entry_continue_open",
        json!({
            "nonce": fields.nonce,
            "request_id": fields.request_id,
        }),
    ) {
        Ok(value) => value,
        Err(error) => return daemon_refusal(&error),
    };
    let token = value["token"].as_str().unwrap_or_default();
    let key = value["key"].as_str().unwrap_or_default();
    if value["request_id"].as_str() != Some(fields.request_id.as_str())
        || !hex64(token)
        || !hex64(key)
        || !value["session"].is_object()
    {
        return auth_refusal(503);
    }
    let Some(document) = entry_document(
        opts,
        Some(EntryBootstrap {
            key,
            request_id: &fields.request_id,
        }),
    ) else {
        return err_response(
            503,
            "no SPA build — pass --dist or rebuild with --features ui",
        );
    };
    let mut response = document_response(document, 200);
    let cookie = format!(
        "{}={token}; Path=/; HttpOnly; SameSite=Strict; Secure",
        super::operator::entry_cookie_name(opts)
    );
    if let Ok(header) = Header::from_bytes("Set-Cookie", cookie.as_bytes()) {
        response.add_header(header);
    }
    response
}

/// Read `conversation`/`limit` from the query and nothing else.
fn list_params(request: &Request) -> Result<serde_json::Value, HttpResp> {
    let mut params = serde_json::Map::new();
    if let Some((_, query)) = request.url().split_once('?') {
        for pair in query.split('&').filter(|pair| !pair.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            match key {
                "conversation" => {
                    let decoded = super::pct_decode(value).ok_or_else(|| {
                        wire_refusal(
                            400,
                            "invalid_request",
                            "This request could not be read. Nothing was sent.",
                        )
                    })?;
                    params.insert("conversation_id".to_string(), json!(decoded));
                }
                "limit" => {
                    let parsed: i64 = value.parse().map_err(|_| {
                        wire_refusal(
                            400,
                            "invalid_request",
                            "This request could not be read. Nothing was sent.",
                        )
                    })?;
                    params.insert("limit".to_string(), json!(parsed.clamp(1, 50)));
                }
                _ => {
                    return Err(wire_refusal(
                        400,
                        "invalid_request",
                        "This request could not be read. Nothing was sent.",
                    ))
                }
            }
        }
    }
    Ok(serde_json::Value::Object(params))
}

/// Parse `attempt_id` and nothing else from the status query.
fn management_status_params(request: &Request) -> Result<serde_json::Value, HttpResp> {
    let mut attempt_id = None;
    if let Some((_, query)) = request.url().split_once('?') {
        for pair in query.split('&').filter(|pair| !pair.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            if key != "attempt_id" {
                return Err(wire_refusal(
                    400,
                    "invalid_request",
                    "This request could not be read. Nothing was sent.",
                ));
            }
            let decoded = super::pct_decode(value).ok_or_else(|| {
                wire_refusal(
                    400,
                    "invalid_request",
                    "This request could not be read. Nothing was sent.",
                )
            })?;
            if decoded.len() != 36 || uuid::Uuid::parse_str(&decoded).is_err() {
                return Err(wire_refusal(
                    400,
                    "invalid_request",
                    "This request could not be read. Nothing was sent.",
                ));
            }
            attempt_id = Some(decoded);
        }
    }
    let attempt_id = attempt_id.ok_or_else(|| {
        wire_refusal(
            400,
            "invalid_request",
            "This request could not be read. Nothing was sent.",
        )
    })?;
    Ok(json!({"attempt_id": attempt_id}))
}

/// The metadata read side (never mints a nonce, never mutates).
pub(super) fn handle_read(
    request: &mut Request,
    state: &Path,
    opts: &ServeOpts,
    route: Route<'_>,
) -> HttpResp {
    let (method, params) = match route {
        Route::Collection => (
            "credential_request_list",
            match list_params(request) {
                Ok(params) => params,
                Err(response) => return response,
            },
        ),
        Route::Show(id) => ("credential_request_show", json!({"request_id": id})),
        Route::ManagementStatus => (
            "protected_management_status",
            match management_status_params(request) {
                Ok(params) => params,
                Err(response) => return response,
            },
        ),
        _ => return err_response(405, "method not allowed"),
    };
    let prepare_id = match route {
        Route::Prepare => params["request_id"].as_str().map(str::to_string),
        _ => None,
    };
    let prepare_entry = EntryOrigin::from_opts(opts);
    match client::rpc(state, method, params) {
        Ok(mut value) => {
            if let Some(request_id) = prepare_id.as_deref() {
                if !prepare_reply_ok(&value, request_id, prepare_entry.as_ref()) {
                    return wire_refusal(
                        503,
                        "entry_unavailable",
                        "Protected credential entry answered unexpectedly. Nothing was sent.",
                    );
                }
            }
            if value.to_string().len() > RESULT_CAP {
                return wire_refusal(
                    502,
                    "entry_unavailable",
                    "Protected credential entry answered unexpectedly. Nothing was sent.",
                );
            }
            // The entry link is server-selected: the SPA renders the value
            // this board attached, never one built from its current origin.
            attach_entry_urls(&mut value, opts);
            ok_json(value)
        }
        Err(error) => daemon_refusal(&error),
    }
}

/// The write side: create/session/submit/cancel. The write dispatcher has
/// already run the board's operator gate; the daemon re-proves the human.
pub(super) fn handle_write(
    request: &mut Request,
    state: &Path,
    opts: &ServeOpts,
    route: Route<'_>,
) -> HttpResp {
    let (method, params) = match route {
        Route::Collection => {
            let body: CreateBody = match typed(request, BODY_CAP) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let value = serde_json::to_value(body).unwrap_or_else(|_| json!({}));
            ("credential_request_create", value)
        }
        Route::EntrySession(id) => {
            if let Err(response) = empty_body(request) {
                return response;
            }
            let proof = match entry_origin_proof(request, opts) {
                Ok(proof) => proof,
                Err(response) => return response,
            };
            (
                "credential_entry_session",
                json!({"request_id": id, "session": proof}),
            )
        }
        Route::Submit(id) => {
            // Prove the entry-origin session BEFORE reading the secret
            // body: a missing/board-origin session never collects bytes.
            let proof = match entry_origin_proof(request, opts) {
                Ok(proof) => proof,
                Err(response) => return response,
            };
            let body: SubmitBody = match typed(request, SUBMIT_CAP) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let mut params = json!({
                "request_id": id,
                "nonce": body.nonce,
                "session": proof,
            });
            if let Some(secret) = body.secret {
                params["secret"] = json!(secret);
            }
            if let Some(transport) = body.transport {
                params["transport"] = serde_json::to_value(transport).unwrap_or_else(|_| json!({}));
            }
            ("credential_request_submit", params)
        }
        Route::Cancel(id) => {
            if let Err(response) = empty_body(request) {
                return response;
            }
            ("credential_request_cancel", json!({"request_id": id}))
        }
        Route::Prepare => {
            let body: PrepareBody = match typed(request, BODY_CAP) {
                Ok(body) => body,
                Err(response) => return response,
            };
            if !body.valid() {
                return wire_refusal(
                    400,
                    "invalid_request",
                    "This request could not be accepted. Nothing was sent.",
                );
            }
            let proof = match board_session_proof(request, opts) {
                Ok(proof) => proof,
                Err(response) => return response,
            };
            (
                "credential_entry_continue_mint",
                json!({"request_id": body.request_id, "session": proof}),
            )
        }
        Route::ManagementSession => {
            let body: ManagementSessionBody = match typed(request, BODY_CAP) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let proof = match board_session_proof(request, opts) {
                Ok(proof) => proof,
                Err(response) => return response,
            };
            (
                "protected_management_session",
                json!({"action": body.action, "params": body.params, "session": proof}),
            )
        }
        Route::ManagementAction => {
            let body: ManagementActionBody = match typed(request, BODY_CAP) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let proof = match board_session_proof(request, opts) {
                Ok(proof) => proof,
                Err(response) => return response,
            };
            (
                "protected_management_action",
                json!({
                    "attempt_id": body.attempt_id,
                    "nonce": body.nonce,
                    "action": body.action,
                    "params": body.params,
                    "session": proof,
                }),
            )
        }
        Route::Show(_) | Route::ManagementStatus => return err_response(405, "method not allowed"),
    };
    let prepare_id = match route {
        Route::Prepare => params["request_id"].as_str().map(str::to_string),
        _ => None,
    };
    let prepare_entry = EntryOrigin::from_opts(opts);
    match client::rpc(state, method, params) {
        Ok(mut value) => {
            if let Some(request_id) = prepare_id.as_deref() {
                if !prepare_reply_ok(&value, request_id, prepare_entry.as_ref()) {
                    return wire_refusal(
                        503,
                        "entry_unavailable",
                        "Protected credential entry answered unexpectedly. Nothing was sent.",
                    );
                }
            }
            if value.to_string().len() > RESULT_CAP {
                return wire_refusal(
                    502,
                    "entry_unavailable",
                    "Protected credential entry answered unexpectedly. Nothing was sent.",
                );
            }
            attach_entry_urls(&mut value, opts);
            ok_json(value)
        }
        Err(error) => daemon_refusal(&error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_routes_and_canonical_request_ids() {
        let id = "0b9c53a1-6f0e-4b7a-9a4e-1f4d7f4f1a2b";
        assert!(matches!(
            route(&format!("/api/credential-requests/{id}/submit")),
            Some(Route::Submit(_))
        ));
        assert!(matches!(
            route(&format!("/api/credential-requests/{id}/session")),
            Some(Route::EntrySession(_))
        ));
        assert!(matches!(
            route(&format!("/api/credential-requests/{id}/cancel")),
            Some(Route::Cancel(_))
        ));
        assert!(matches!(
            route(&format!("/api/credential-requests/{id}")),
            Some(Route::Show(_))
        ));
        assert!(matches!(
            route("/api/credential-requests"),
            Some(Route::Collection)
        ));
        for bad in [
            "/api/credential-requests/../submit",
            "/api/credential-requests/not-a-uuid",
            "/api/credential-requests/0B9C53A1-6F0E-4B7A-9A4E-1F4D7F4F1A2B",
            &format!("/api/credential-requests/{id}/submit/extra"),
            &format!("/api/credential-requests/{id}/rotate"),
        ] {
            assert!(route(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn bodies_are_strict_and_never_coerce() {
        assert!(serde_json::from_str::<SubmitBody>(r#"{"nonce":"n"}"#).is_ok());
        assert!(serde_json::from_str::<SubmitBody>(r#"{"nonce":"n","secret":"s"}"#).is_ok());
        for bad in [
            r#"{"nonce":"n","secret":null}"#,
            r#"{"nonce":"n","secret":3}"#,
            r#"{"nonce":"n","credential":"s"}"#,
            r#"{"nonce":"n","nonce":"m"}"#,
        ] {
            assert!(serde_json::from_str::<SubmitBody>(bad).is_err(), "{bad}");
        }
        assert!(serde_json::from_str::<Empty>("{}").is_ok());
        assert!(serde_json::from_str::<Empty>(r#"{"request_id":"x"}"#).is_err());
        assert!(serde_json::from_str::<CreateBody>(
            r#"{"provider":"smtp","verb":"enroll","shape":"smtp"}"#
        )
        .is_ok());
        // The create body never carries a credential, account, transport or
        // caller identity — those live only on the isolated submit form.
        for bad in [
            r#"{"provider":"smtp","verb":"enroll","secret":"x"}"#,
            r#"{"provider":"smtp","verb":"enroll","account":"a"}"#,
            r#"{"provider":"smtp","verb":"enroll","host":"h"}"#,
            r#"{"provider":"smtp","verb":"enroll","session_binding":"b"}"#,
            r#"{"provider":"smtp","verb":"enroll","workspace_id":"w"}"#,
        ] {
            assert!(serde_json::from_str::<CreateBody>(bad).is_err(), "{bad}");
        }
        assert!(serde_json::from_str::<SubmitBody>(
            r#"{"nonce":"n","secret":"s","transport":{"host":"h","port":465,"tls_mode":"implicit","username":"u","sender":"a@b"}}"#
        )
        .is_ok());
        assert!(
            serde_json::from_str::<SubmitBody>(r#"{"nonce":"n","session_binding":"b"}"#).is_err()
        );
        assert!(serde_json::from_str::<SubmitBody>(
            r#"{"nonce":"n","transport":{"host":"h","port":465,"tls_mode":"implicit","username":"u","sender":"a@b","extra":"x"}}"#
        )
        .is_err());
    }

    #[test]
    fn refusals_are_fixed_and_do_not_reflect_daemon_text() {
        let raw = Error::invalid(
            "request_changed",
            "connection conn-secret host mail.example",
        );
        let response = daemon_refusal(&raw);
        assert_eq!(response.status_code().0, 409);
        let unknown = daemon_refusal(&Error::internal("host mail.example leaked"));
        assert_eq!(unknown.status_code().0, 503);
    }
}
