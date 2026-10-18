//! CAD-1006: the board's private screen mount — the operator mint POST
//! and the nonce-consumed frame GET that serves the CSP-pinned document.
//!
//! Two routes, two different authorities — and the session is proven by
//! relaying the request's real credential to the daemon's native check,
//! never by trusting a caller-supplied id:
//!
//! - `POST /api/app-installations/<id>/screens/<tag>/mount` —
//!   `RouteClass::ScreenRead` (CAD-1537: a member or `--read-only` board
//!   mounts read-only), body `{}`. The board takes the request's
//!   session cookie token + `X-Cadence-Session` key (the same credentials
//!   `check_session` already proves) and relays them to `app_screen_mint`,
//!   which runs `Auth::check`/`check_public` itself and binds the minted
//!   cap to the *resolved* session id. Returns `{mount: path}`.
//!
//! - `GET /api/app-screen/<nonce>` — the frame document. A navigation
//!   is headerless (no `X-Cadence-Session` page key), so it never runs
//!   `admit_operator_read` and relays ONLY the nonce to
//!   `app_screen_consume`: its sole authority is the burned one-use
//!   FrameCap PLUS the daemon's `operator_connection` peer guard (denying
//!   agent/detached callers) PLUS the stored mint-verified session id
//!   re-proved live in `Auth`, PLUS the live digest/approval/package
//!   re-check. No cookie, key, or session field is read on this path and
//!   no session material reaches the child or a log.
//!
//! The rendered document carries its own CSP (nonce-pinned script,
//! `frame-ancestors` = this board's origin) — the board CSP is unchanged.

use serde_json::{json, Value};
use tiny_http::{Header, Request, Response, StatusCode};

use super::{err_response, home, operator, read_body, HttpResp, ServeOpts};
use crate::client;
use crate::operator_auth::Origin;

const BODY_CAP: u64 = 4 * 1024;

/// `POST /api/app-installations/<id>/screens/<tag>/mount` → `(id, tag)`.
pub(super) fn mount_route(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("/api/app-installations/")?;
    let (install, rest) = rest.split_once("/screens/")?;
    let (tag, verb) = rest.split_once('/')?;
    if verb != "mount" || install.is_empty() || install.contains('/') {
        return None;
    }
    if !crate::issue::app_screen_pkg::valid_tag(tag) {
        return None;
    }
    if install.len() > 128
        || !install
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return None;
    }
    Some((install, tag))
}

/// `GET /api/app-screen/<nonce>` → `Some(nonce)` for exactly that shape.
pub(super) fn frame_route(path: &str) -> Option<&str> {
    let nonce = path.strip_prefix("/api/app-screen/")?;
    if nonce.is_empty() || nonce.contains('/') || nonce.len() != 64 {
        return None;
    }
    Some(nonce)
}

/// The request's own session credential pieces for the MINT relay:
/// `(token, key, origin)` — the cookie's value, the `X-Cadence-Session`
/// key, and the request's classified origin. `None` when the request
/// presents no session cookie for its origin — refused upstream by the
/// daemon's check either way. Used ONLY by the mint POST; the frame GET
/// relays nothing (the nonce is its sole authority).
pub(super) fn request_credentials(
    request: &Request,
    opts: &ServeOpts,
) -> Option<(String, String, &'static str)> {
    let origin = operator::request_origin_kind(request, opts)?;
    let token = operator::session_token_for(request, opts, origin)?;
    let key = super::header_value(request, operator::SESSION_HEADER).unwrap_or_default();
    Some((token, key, origin.as_str()))
}

/// The mint handler — body `{context_id?}` only; install/tag are from the
/// URL and the optional context is re-proven by the daemon for that install.
/// Session credentials are relayed from the proven request. The child frame
/// never supplies or receives this authority.
pub(super) fn mount(
    request: &mut Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
    install_id: &str,
    tag: &str,
) -> HttpResp {
    let bytes = match read_body(request, BODY_CAP) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let body: Value = match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) if v.is_object() => v,
        _ => return err_response(400, "screen mount body must be an object"),
    };
    if body.as_object().unwrap().keys().any(|k| k != "context_id") {
        return err_response(400, "screen mount has unsupported fields");
    }
    let context_id = body.get("context_id").and_then(Value::as_str).unwrap_or("");
    if context_id.len() > 128 {
        return err_response(400, "screen context ID exceeds its bound");
    }
    let Some((token, key, origin)) = request_credentials(request, opts) else {
        return err_response(403, "screen mount needs a live operator session");
    };
    let generation = next_generation();
    let mut params = json!({
        "install_id": install_id,
        "context_id": if context_id.is_empty() { Value::Null } else { json!(context_id) },
        "tag": tag,
        "token": token,
        "key": key,
        "origin": origin,
        "generation": generation,
    });
    // CAD-1537: the `--read-only` board can only narrow its mounts; the
    // daemon makes a member's mount read-only on its own.
    if opts.read_only {
        params["read_only"] = json!(true);
    }
    match client::rpc(state_dir, "app_screen_mint", params) {
        Ok(v) => super::json_response(v),
        Err(e) => home::rpc_err(&e, "app_screen_mint"),
    }
}

/// Closed trusted-host service endpoints. Credentials come ONLY from the
/// admitted request, never from its JSON body or the child frame.
pub(super) fn service_route(path: &str) -> Option<&'static str> {
    match path {
        "/api/app-screen-services/call" => Some("app_screen_service_call"),
        "/api/app-screen-services/slot-prepare" => Some("app_screen_service_slot_prepare"),
        "/api/app-screen-services/slot-confirm" => Some("app_screen_service_slot_confirm"),
        _ => None,
    }
}

// Value's ordinary parser collapses duplicate fields. This visitor preserves
// the closed service contract by refusing duplicates at every JSON level.
struct UniqueServiceJson(Value);
impl<'de> serde::Deserialize<'de> for UniqueServiceJson {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = UniqueServiceJson;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON without duplicate fields")
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(UniqueServiceJson(json!(v)))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(UniqueServiceJson(json!(v)))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(UniqueServiceJson(json!(v)))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
                Ok(UniqueServiceJson(json!(v)))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(UniqueServiceJson(json!(v)))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(UniqueServiceJson(json!(v)))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueServiceJson(Value::Null))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> Result<Self::Value, A::Error> {
                let mut items = Vec::new();
                while let Some(UniqueServiceJson(v)) = a.next_element()? {
                    items.push(v);
                }
                Ok(UniqueServiceJson(Value::Array(items)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut a: A,
            ) -> Result<Self::Value, A::Error> {
                let mut fields = serde_json::Map::new();
                while let Some(k) = a.next_key::<String>()? {
                    if fields.contains_key(&k) {
                        return Err(serde::de::Error::custom("duplicate service field"));
                    }
                    let UniqueServiceJson(v) = a.next_value()?;
                    fields.insert(k, v);
                }
                Ok(UniqueServiceJson(Value::Object(fields)))
            }
        }
        d.deserialize_any(Visitor)
    }
}

/// `read_only` (CAD-1537): a member or `--read-only` board caller — the
/// board refuses a non-read operation itself before relaying, as strict
/// as the daemon's own read-only mount rule.
pub(super) fn service(
    request: &mut Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
    method: &'static str,
    read_only: bool,
) -> HttpResp {
    // One finite read budget before bounded envelope inspection. The named
    // operation then imposes its smaller exact raw-envelope + args budget.
    let bytes = match read_body(request, 327_680 + 4096) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let mut body = match serde_json::from_slice::<UniqueServiceJson>(&bytes) {
        Ok(UniqueServiceJson(v)) if v.is_object() => v,
        _ => {
            return err_response(
                400,
                "screen service body must be duplicate-free JSON object",
            )
        }
    };
    let confirm = method == "app_screen_service_slot_confirm";
    let allowed = if confirm {
        &["action_token", "confirmation_token", "confirmation"][..]
    } else {
        &["action_token", "operation", "args"][..]
    };
    if body
        .as_object()
        .unwrap()
        .keys()
        .any(|key| !allowed.contains(&key.as_str()))
    {
        return err_response(400, "screen service body has unsupported fields");
    }
    let cap = if confirm {
        16_384
    } else {
        match serde_json::from_value::<crate::issue::app_screen_decl::ScreenOperation>(
            body["operation"].clone(),
        ) {
            Ok(operation) if read_only && !operation.is_screen_read() => {
                return super::coded_response(
                    403,
                    "forbidden",
                    "a read-only screen admits only read operations",
                    None,
                )
            }
            Ok(operation) => operation.args_limit(),
            Err(_) => return err_response(400, "unknown screen service operation"),
        }
    };
    let args = if confirm {
        &body["confirmation"]
    } else {
        &body["args"]
    };
    if bytes.len() > cap + 4096 || !args.is_object() || args.to_string().len() > cap {
        return err_response(400, "screen service request exceeds operation byte bound");
    }
    let Some((token, key, origin)) = request_credentials(request, opts) else {
        return super::coded_response(
            403,
            "forbidden",
            "screen service requires live operator session",
            None,
        );
    };
    let operation = body
        .get("operation")
        .and_then(Value::as_str)
        .map(str::to_owned);
    body["token"] = json!(token);
    body["key"] = json!(key);
    body["origin"] = json!(origin);
    match client::rpc(state_dir, method, body) {
        Ok(v) => service_json_response(&v),
        Err(error)
            if error.kind() == "rejected" && error.code() == Some("screen_access_denied") =>
        {
            super::coded_response(403, "forbidden", "screen service access unavailable", None)
        }
        Err(error)
            if error.kind() == "rejected"
                && method == "app_screen_service_call"
                && matches!(
                    (operation.as_deref(), error.code()),
                    (Some("audience.show"), Some("app_audience_freeze_missing"))
                        | (Some("content.show"), Some("app_content_missing"))
                ) =>
        {
            super::coded_response(404, "not_found", &error.to_string(), None)
        }
        Err(error) if error.kind() == "conflict" => {
            super::coded_response(409, "conflict", &error.to_string(), error.revision())
        }
        Err(error) => home::rpc_err(&error, method),
    }
}

/// Serialize once: the exact compact wire bytes, not a compact estimate
/// followed by pretty JSON. Private service/confirmation results are never
/// cacheable. Error responses retain the existing refusal/status mapping.
fn service_json_response(value: &Value) -> HttpResp {
    let bytes = match serde_json::to_vec(value) {
        Ok(bytes) if bytes.len() <= 1_048_576 => bytes,
        Ok(_) => return err_response(502, "screen service response exceeds byte bound"),
        Err(_) => return err_response(502, "screen service response serialization failed"),
    };
    let mut response = Response::from_data(bytes).with_status_code(StatusCode(200));
    response.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    response.add_header(Header::from_bytes("Vary", "Cookie, X-Cadence-Session").unwrap());
    response.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    response
}

/// `POST /api/app-screen-tools/invoke` — the standalone tool call the
/// trusted host relays for a live mounted frame. Body
/// `{action_token, tool_alias, input, request_id}`. The host API adapter maps
/// the child-facing `alias` selector to `tool_alias`. The board relays the
/// request's own session credentials (the same `check_session` proof the
/// mount mint uses); the daemon re-proves the session AND the
/// server-bound mount action context before any broker/provider work.
/// The frame itself never reaches this route (`connect-src 'none'`).
pub(super) fn tool_route(path: &str) -> bool {
    path == "/api/app-screen-tools/invoke"
}

pub(super) fn tool_invoke(
    request: &mut Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
) -> HttpResp {
    let bytes = match read_body(request, BODY_CAP) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let body: Value = match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) if v.is_object() => v,
        _ => return err_response(400, "tool invoke body must be a JSON object"),
    };
    // Closed body shape the daemon re-checks; the session fields are
    // added host-side from the proven request, never from the body.
    let alias = body.get("tool_alias").and_then(Value::as_str).unwrap_or("");
    let action_token = body
        .get("action_token")
        .and_then(Value::as_str)
        .unwrap_or("");
    let request_id = body.get("request_id").and_then(Value::as_str).unwrap_or("");
    let input = body.get("input").cloned().unwrap_or_else(|| json!({}));
    let generation_scope = body.get("generation_scope").cloned();
    if body.as_object().is_some_and(|o| {
        o.keys().any(|k| {
            !matches!(
                k.as_str(),
                "action_token" | "tool_alias" | "input" | "request_id" | "generation_scope"
            )
        })
    }) {
        return err_response(400, "tool invoke body contains unsupported fields");
    }
    if alias.is_empty() || action_token.is_empty() || request_id.is_empty() || !input.is_object() {
        return err_response(
            400,
            "tool invoke needs action_token, alias, request_id and object input",
        );
    }
    let Some((token, key, origin)) = request_credentials(request, opts) else {
        return err_response(403, "tool invoke needs a live operator session");
    };
    let mut params = json!({"action_token":action_token,"tool_alias":alias,"input":input,"request_id":request_id,"token":token,"key":key,"origin":origin});
    if let Some(scope) = generation_scope {
        params["generation_scope"] = scope;
    }
    match client::rpc(state_dir, "app_tool_invoke", params) {
        Ok(v) => super::json_response(v),
        Err(e) => home::rpc_err(&e, "app_tool_invoke"),
    }
}

/// `POST /api/app-screen-tools/revoke` — the host tearing a mount down
/// revokes its action handle. Body `{action_token}`. OperatorOnly; the
/// board relays the request's own session so the daemon only revokes a
/// context that session owns. Idempotent — tearing down an already-gone
/// mount is a no-op, never an error.
pub(super) fn tool_revoke_route(path: &str) -> bool {
    path == "/api/app-screen-tools/revoke"
}

pub(super) fn tool_revoke(
    request: &mut Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
) -> HttpResp {
    let bytes = match read_body(request, BODY_CAP) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let body: Value = match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) if v.is_object() => v,
        _ => return err_response(400, "tool revoke body must be a JSON object"),
    };
    let action_token = body
        .get("action_token")
        .and_then(Value::as_str)
        .unwrap_or("");
    if action_token.is_empty() {
        return err_response(400, "tool revoke needs action_token");
    }
    let Some((token, key, origin)) = request_credentials(request, opts) else {
        return err_response(403, "tool revoke needs a live operator session");
    };
    match client::rpc(
        state_dir,
        "app_tool_revoke",
        json!({
            "action_token": action_token,
            "token": token,
            "key": key,
            "origin": origin,
        }),
    ) {
        Ok(v) => super::json_response(v),
        Err(e) => home::rpc_err(&e, "app_tool_revoke"),
    }
}

/// `GET /api/app-screen-tools/results?install_id=<id>` — operator read of
/// retained standalone tool receipts for one installation (the projection
/// reads them to restore posts after reload). Operator-read gated like
/// every app read; relays to `app_tool_results`.
pub(super) fn tool_results_route(path: &str) -> bool {
    path == "/api/app-screen-tools/results"
}

pub(super) fn tool_results(request: &Request, state_dir: &std::path::Path) -> HttpResp {
    let query = request.url().split_once('?').map(|(_, q)| q).unwrap_or("");
    let params: std::collections::HashMap<&str, &str> = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .collect();
    let install_id = params.get("install_id").map(|v| v.to_string());
    let context_id = params.get("context_id").map(|v| v.to_string());
    let Some(install_id) = install_id.filter(|id| !id.is_empty()) else {
        return err_response(400, "tool results needs an install_id");
    };
    let params = if let Some(context) = context_id {
        json!({"install_id":install_id,"context_id":context})
    } else {
        json!({"install_id":install_id})
    };
    match client::rpc(state_dir, "app_tool_results", params) {
        Ok(v) => super::json_response(v),
        Err(e) => home::rpc_err(&e, "app_tool_results"),
    }
}

/// The frame GET — the nonce is the SOLE authority. The board relays
/// ONLY the nonce to `app_screen_consume`; the daemon's own
/// `operator_connection` peer guard denies a registered-agent/detached
/// caller, then burns the cap and re-proves the minting session is live
/// and the digest/approval/package unchanged. No cookie, key, or session
/// field is read or forwarded on this path.
pub(super) fn frame(
    request: &Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
    nonce: &str,
) -> HttpResp {
    let out = match client::rpc(state_dir, "app_screen_consume", json!({"nonce": nonce})) {
        Ok(v) => v,
        Err(e) => {
            let msg = e.to_string();
            let code = if msg.contains("spent or unknown")
                || msg.contains("expired")
                || msg.contains("invalid screen capability")
            {
                404
            } else if msg.contains("not approved") || msg.contains("digest changed") {
                409
            } else {
                403
            };
            return err_response(code, "screen mount refused");
        }
    };
    let csp_nonce = fresh_csp_nonce();
    // The bridge nonce is the DISTINCT non-authorizing token the mint
    // bound into this cap and returned to the host; render THAT stored
    // value so the host recognizes the child's echoed init. Never mint a
    // fresh one at render and never accept a child-chosen nonce.
    let bridge_nonce = out["bridge_nonce"].as_str().unwrap_or("").to_string();
    // Rebuild from the native manifest + exact string bodies, never from a
    // host/frame-selected script vector or an order inferred from file names.
    let package = (|| -> crate::error::Result<crate::issue::app_screen_pkg::ScreenPackage> {
        use crate::error::Error;
        let manifest = out["manifest"]
            .as_str()
            .ok_or_else(|| Error::rejected("screen manifest unavailable"))?;
        let assets = out["assets"]
            .as_object()
            .ok_or_else(|| Error::rejected("screen assets unavailable"))?
            .iter()
            .map(|(name, value)| {
                value
                    .as_str()
                    .map(|text| (name.clone(), text.to_owned()))
                    .ok_or_else(|| Error::rejected("screen asset is not text"))
            })
            .collect::<crate::error::Result<std::collections::BTreeMap<_, _>>>()?;
        let checked = crate::issue::app_screen_decl::validate_map(manifest, &assets)?;
        Ok(crate::issue::app_screen_pkg::ScreenPackage {
            tag: out["tag"]
                .as_str()
                .ok_or_else(|| Error::rejected("screen tag unavailable"))?
                .to_owned(),
            manifest: manifest.to_owned(),
            assets,
            app: checked.app().to_owned(),
            remote_images: checked.remote_images().to_vec(),
            contract: checked.contract().to_owned(),
            may: checked.may().to_vec(),
            tools: checked.tools().clone(),
            host_contract: checked.host_contract().map(str::to_owned),
            operations: checked.operations().to_vec(),
            navigation: checked.navigation().cloned(),
        })
    })();
    let package = match package {
        Ok(package) => package,
        Err(_) => return err_response(500, "screen package refused"),
    };
    let origin_str = board_origin(request, opts);
    let generation = out["generation"].as_u64().unwrap_or(0);
    let tag = out["tag"].as_str().unwrap_or("");
    let html = match crate::daemon::render_package_frame_html(
        &csp_nonce,
        tag,
        &bridge_nonce,
        generation,
        &package,
        &origin_str,
    ) {
        Ok(h) => h,
        Err(e) => return err_response(500, &e.to_string()),
    };
    let mut resp = Response::from_data(html.into_bytes()).with_status_code(StatusCode(200));
    resp.add_header(Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap());
    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    resp.add_header(Header::from_bytes("Referrer-Policy", "no-referrer").unwrap());
    resp.add_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
    let csp = frame_csp(&csp_nonce, &origin_str, &package.remote_images);
    resp.add_header(Header::from_bytes("Content-Security-Policy", csp).unwrap());
    resp
}

/// The frame document's CSP. `img-src` is `data:` only, plus — for a
/// screen whose verified declaration opts into a host-listed remote
/// image set (CAD-1123, operator decision Q1) — exactly that set's
/// https origins, so the browser loads them straight into the frame.
/// The daemon never fetches them, and `connect-src` stays `'none'`. A
/// name the host does not list adds nothing.
fn frame_csp(csp_nonce: &str, origin: &str, remote_images: &[String]) -> String {
    let mut img_src = String::from("data:");
    for name in remote_images {
        for host in crate::issue::app_screen_decl::remote_image_sources(name).unwrap_or(&[]) {
            if !img_src.split(' ').any(|have| have == *host) {
                img_src.push(' ');
                img_src.push_str(host);
            }
        }
    }
    format!(
        "default-src 'none'; script-src 'nonce-{csp_nonce}'; style-src 'unsafe-inline'; \
         img-src {img_src}; font-src data:; connect-src 'none'; media-src 'none'; \
         object-src 'none'; frame-src 'none'; worker-src 'none'; base-uri 'none'; \
         form-action 'none'; frame-ancestors {origin}"
    )
}

/// The board's own origin for `frame-ancestors` and the child's
/// `postMessage` target — derived from the TRUSTED origin classification
/// (`request_origin_kind`), never the raw `Host` header or a forwarded
/// `X-Forwarded-Proto` (an arbitrary Host/scheme must not steer the
/// origin the child is told to trust). Each classified origin resolves
/// to the board's canonical name for it:
/// - `Origin::Public` → `{public_scheme(public.host)}://{public.host}`
///   (https in production, http only when the configured public host is
///   `*.localhost`) via [`operator::public_scheme`];
/// - `Origin::Tailnet` → [`super::tailnet_url`] of the configured
///   `(dns_name, https_port)` (the `:443` is omitted, others kept);
/// - loopback/`NoSession` → `http://<board_host(port)>` — the canonical
///   loopback name the frame is always reached on, so `frame-ancestors`
///   still pins exactly.
fn board_origin(request: &Request, opts: &ServeOpts) -> String {
    origin_for(operator::request_origin_kind(request, opts), opts)
}

/// The canonical origin for a classified request origin — pure, so the
/// per-class resolution is unit-testable without a live `Request`.
/// A fresh 128-bit CSP nonce, base64url no-pad — one per document.
fn fresh_csp_nonce() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("getrandom");
    b64url(&bytes)
}

fn b64url(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(T[((n >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(T[(n & 63) as usize] as char);
        }
    }
    out
}

/// Monotonic mount generation for this board process — defense in depth
/// behind the nonce so a replayed mint never reuses a mount identity.
fn next_generation() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static GEN: AtomicU64 = AtomicU64::new(1);
    GEN.fetch_add(1, Ordering::SeqCst)
}

fn origin_for(class: Option<Origin>, opts: &ServeOpts) -> String {
    match class {
        Some(Origin::Public) => match &opts.public {
            Some(public) => {
                format!(
                    "{}://{}",
                    operator::public_scheme(&public.host),
                    public.host
                )
            }
            None => format!("http://{}", operator::board_host(opts.port)),
        },
        Some(Origin::Tailnet) => match &opts.tailnet {
            Some((dns_name, https_port)) => super::tailnet_url(dns_name, *https_port),
            None => format!("http://{}", operator::board_host(opts.port)),
        },
        _ => format!("http://{}", operator::board_host(opts.port)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(public: Option<crate::ui::PublicBoard>, tailnet: Option<(String, u16)>) -> ServeOpts {
        ServeOpts {
            port: 8080,
            public,
            tailnet,
            ..Default::default()
        }
    }

    fn public_board(host: &str) -> crate::ui::PublicBoard {
        crate::ui::PublicBoard {
            host: host.to_string(),
            issuer: "https://issuer".into(),
            company: "c".into(),
            authorize_url: "https://app/x".into(),
        }
    }

    #[test]
    fn origin_for_uses_canonical_names_not_request_host() {
        // Tailnet: the configured dns_name + https_port; :443 omitted, a
        // non-default port kept.
        let t = opts(None, Some(("box.tail-abc.ts.net".into(), 443)));
        assert_eq!(
            origin_for(Some(Origin::Tailnet), &t),
            "https://box.tail-abc.ts.net"
        );
        let t9460 = opts(None, Some(("box.tail-abc.ts.net".into(), 9460)));
        assert_eq!(
            origin_for(Some(Origin::Tailnet), &t9460),
            "https://box.tail-abc.ts.net:9460"
        );
        // Public: production host is https; a configured *.localhost is http.
        let prod = opts(Some(public_board("acme.cadencecloud.app")), None);
        assert_eq!(
            origin_for(Some(Origin::Public), &prod),
            "https://acme.cadencecloud.app"
        );
        let local = opts(Some(public_board("acme.board.localhost:3123")), None);
        assert_eq!(
            origin_for(Some(Origin::Public), &local),
            "http://acme.board.localhost:3123"
        );
        // Loopback/absent classification -> the board's own loopback name.
        let plain = opts(None, None);
        assert_eq!(
            origin_for(Some(Origin::Loopback), &plain),
            format!("http://{}", operator::board_host(8080))
        );
        assert_eq!(
            origin_for(None, &plain),
            format!("http://{}", operator::board_host(8080))
        );
    }

    #[test]
    fn cad1123_csp_admits_instagram_cdn_only_for_a_declaring_screen() {
        let base = frame_csp("n", "http://h", &[]);
        assert!(base.contains("img-src data:;"), "{base}");
        assert!(!base.contains("cdninstagram") && !base.contains("fbcdn"));
        let declared = frame_csp("n", "http://h", &["instagram-cdn".to_string()]);
        assert!(
            declared.contains("img-src data: https://*.cdninstagram.com https://*.fbcdn.net;"),
            "{declared}"
        );
        // Everything else stays closed: no fetch/XHR egress, no frames.
        assert!(declared.contains("connect-src 'none'") && declared.contains("frame-src 'none'"));
        // A name the host does not list, or a raw host, adds nothing.
        for forged in ["https://evil.example", "*", "instagram-cdn ", "cdn"] {
            let csp = frame_csp("n", "http://h", &[forged.to_string()]);
            assert_eq!(csp, base, "unlisted image set widened the CSP: {forged}");
        }
        // A repeated declaration does not repeat hosts.
        let twice = frame_csp(
            "n",
            "http://h",
            &["instagram-cdn".to_string(), "instagram-cdn".to_string()],
        );
        assert_eq!(twice, declared);
    }
}
