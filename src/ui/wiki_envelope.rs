//! CAD-1401: the board's verified wiki-envelope route family. AgenticOS's
//! `/__platform/wiki/call` proxy (AOS-225) translates the contract's
//! actions onto these container routes and keeps the `wikienv_` bearer:
//!
//! - enumerate → `GET /api/wiki/ls?path=` or `GET /api/wiki/search?q=&path=`
//! - read      → `GET /api/wiki/file?path=` with optional `offset`, `bytes`, and `rev`
//! - write     → `PUT /api/wiki/file` JSON `{path,text,if_rev?}`
//! - upload    → `POST /api/wiki/upload?path=` single-file multipart
//!
//! Only a request bearing a `wikienv_` bearer enters here. A browser
//! request never does, so browser routes and their CSRF/session gates are
//! untouched. The bearer is verified independently against this board's
//! own pins ([`crate::wiki::envelope`]); an envelope never reaches a
//! handler unverified, and no request field names authority.
//!
//! Every read is narrowed to the signed `prefix_grants` ceiling for its
//! scope, listings and search hits included, not only the requested path.
//! The daemon still enforces `wiki::allowed` and attributes the call via
//! `wiki_as`; this route can only shrink what the daemon would allow.

use serde_json::{json, Value};
use tiny_http::{Method, Request, Response, StatusCode};

use super::write_path::{header_value, parse_json, read_body, write_err, HttpResp};
use super::ServeOpts;
use crate::board_identity;
use crate::cli_actor::{self, Rejection};
use crate::client;
use crate::wiki::envelope::{self, WikiAction, WikiGrant};

/// A refusal in the contract's `error.code` shape, `no-store`, never a
/// redirect or cookie. Mirrors `cli_route`'s refusal shape.
fn refuse(status: u16, code: &str, message: &str) -> HttpResp {
    let body = serde_json::to_vec(&json!({
        "ok": false,
        "error": {"code": code, "message": message},
    }))
    .unwrap_or_default();
    let mut resp = Response::from_data(body).with_status_code(StatusCode(status));
    resp.add_header(tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap());
    resp.add_header(tiny_http::Header::from_bytes("Cache-Control", "no-store").unwrap());
    resp
}

fn status_of(code: &str) -> u16 {
    match code {
        "capability_unavailable" => 503,
        _ => 401,
    }
}

/// True when the request is a wiki-envelope call: a `wikienv_` bearer.
/// Anything else (a cookie session, no bearer, another scheme) is not
/// ours, and the caller continues on the existing browser path.
pub(crate) fn is_envelope_request(request: &Request) -> bool {
    header_value(request, "Authorization")
        .is_some_and(|v| v.trim().contains(cli_actor::BEARER_PREFIX))
}

/// Map a translated (method, path) to the one wiki action it may run.
/// Anything else under `/api/wiki/` is refused before a body is read.
fn action_of(method: &Method, tail: &str) -> Option<WikiAction> {
    match (method, tail) {
        (Method::Get, "ls") | (Method::Get, "search") => Some(WikiAction::Enumerate),
        (Method::Get, "file") => Some(WikiAction::Read),
        (Method::Post, "job_status") => Some(WikiAction::Read),
        (Method::Put, "transfer") => Some(WikiAction::Write),
        (Method::Put, "file") | (Method::Post, "upload") => Some(WikiAction::Write),
        _ => None,
    }
}

/// Verify the envelope bearer into a [`WikiGrant`], with the pins from
/// this board's public identity (never from the envelope or request).
fn grant_of(request: &Request, public: &crate::ui::PublicBoard) -> Result<WikiGrant, HttpResp> {
    let bearer = header_value(request, "Authorization");
    let env = match cli_actor::envelope_from_bearer(bearer.as_deref()) {
        Ok(e) => e,
        Err(r) => return Err(refuse(status_of(r.code), r.code, &r.message)),
    };
    let config = board_identity::Config {
        host: public.host.clone(),
        issuer: public.issuer.clone(),
        company: public.company.clone(),
    };
    let now = crate::issue::time::now_epoch();
    let kid = peek_kid(&env)?;
    let key = {
        let mut cache = CLI_JWKS.lock().unwrap_or_else(|e| e.into_inner());
        match cache.key(&config.issuer, &kid, now) {
            Ok(k) => k,
            Err(e) => {
                let (code, status) = match e.code() {
                    Some("assertion_invalid") => ("assertion_invalid", 401),
                    _ => ("capability_unavailable", 503),
                };
                return Err(refuse(status, code, &e.to_string()));
            }
        }
    };
    envelope::verify(&env, &config, &key, now)
        .map_err(|r: Rejection| refuse(status_of(r.code), r.code, &r.message))
}

/// The signed header's `kid`, peeked for the JWKS lookup only.
fn peek_kid(env: &str) -> Result<String, HttpResp> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let head = env.split('.').next().unwrap_or_default();
    let bytes = URL_SAFE_NO_PAD
        .decode(head)
        .map_err(|_| refuse(401, "assertion_invalid", "envelope is not base64url"))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
        refuse(
            401,
            "bad_header",
            "envelope header is not the contract shape",
        )
    })?;
    Ok(value["kid"].as_str().unwrap_or_default().to_string())
}

/// Process-wide JWKS cache (shared shape with `cli_route`).
static CLI_JWKS: std::sync::Mutex<board_identity::JwksCache> =
    std::sync::Mutex::new(board_identity::JwksCache::new());

/// CAD-1434 / AOS-243 test seam: prime the process-wide JWKS cache through the
/// SAME `JwksCache::key` that `grant_of` calls, so the production `fetch_keys`
/// fetch and parser populate it. There is no key setter: the only way a key
/// enters the cache is the real issuer fetch. The native request thread does
/// not inherit a thread-local TLS fixture, so the controller primes here on its
/// own thread before any native call. Test builds only.
#[cfg(all(test, feature = "test-seam"))]
pub(crate) fn prime_jwks_for_test(issuer: &str, kid: &str, now: i64) -> crate::Result<()> {
    let mut cache = CLI_JWKS.lock().unwrap_or_else(|e| e.into_inner());
    cache.key(issuer, kid, now).map(|_| ())
}

/// `wikienv_` requests on `/api/wiki/<tail>`. Only the translated routes
/// are served; a verified envelope whose scope or prefix does not cover
/// the action is refused before the daemon sees it.
pub(crate) fn handle(
    request: &mut Request,
    method: &Method,
    tail: &str,
    query: &dyn Fn(&str) -> Option<String>,
    state_dir: &std::path::Path,
    pm_dir: &std::path::Path,
    opts: &ServeOpts,
) -> HttpResp {
    let Some(public) = opts.public.as_ref() else {
        return refuse(
            503,
            "capability_unavailable",
            "wiki authority is not configured",
        );
    };
    let Some(action) = action_of(method, tail) else {
        return refuse(404, "not_found", "no such wiki envelope route");
    };
    let grant = match grant_of(request, public) {
        Ok(g) => g,
        Err(resp) => return resp,
    };
    if !grant.allows(action) {
        return refuse(
            403,
            "insufficient_scope",
            "the envelope's granted wiki scopes do not cover this action",
        );
    }
    // Single-use: consume the jti like the CLI route, so a captured
    // wiki bearer cannot be replayed at this route either.
    if !consume_jti(state_dir, &grant) {
        return refuse(
            401,
            "assertion_replayed",
            "the envelope's jti was already consumed",
        );
    }
    // Only a verified `users/` or the owner `operator` maps to a daemon
    // identity. A service or agent envelope has no `wiki_as` claim the
    // daemon accepts, so it is refused rather than relayed as an agent.
    let Some(attribution) = relay_claim(&grant.actor) else {
        return refuse(
            403,
            "insufficient_scope",
            "this envelope's actor has no wiki identity on this board",
        );
    };
    // The board's configured `--read-only` refuses every write route, as
    // `operator::admit` does for browser writes. Checked before the daemon
    // mutation; reads stay allowed.
    if action == WikiAction::Write && opts.read_only {
        return refuse(403, "read_only", "board is read-only — writes are disabled");
    }
    match action {
        WikiAction::Read if tail == "job_status" => {
            transfer(request, state_dir, &grant, &attribution, false)
        }
        WikiAction::Write if tail == "transfer" => {
            transfer(request, state_dir, &grant, &attribution, true)
        }
        WikiAction::Enumerate => enumerate(query, state_dir, &grant, &attribution),
        WikiAction::Read => read(request.url(), query, state_dir, &grant, &attribution),
        WikiAction::Write if tail == "upload" => {
            upload(request, query, state_dir, pm_dir, &grant, &attribution)
        }
        WikiAction::Write => write(request, state_dir, &grant, &attribution),
    }
}

/// The transport's existing write/read scopes cover only the explicit transfer
/// operations. All manifest paths are checked before the daemon sees a write.
fn transfer(
    request: &mut Request,
    state_dir: &std::path::Path,
    grant: &WikiGrant,
    attribution: &str,
    writing: bool,
) -> HttpResp {
    let body = match read_body(request, super::JSON_CAP) {
        Ok(body) => body,
        Err(resp) => return resp,
    };
    let body: Value = match parse_json(&body) {
        Ok(body) => body,
        Err(resp) => return resp,
    };
    let mut params = match super::wiki::transfer_params(&body, writing) {
        Ok(params) => params,
        Err(resp) => return resp,
    };
    let action = if writing {
        WikiAction::Write
    } else {
        WikiAction::Read
    };
    let root = body["path"].as_str().unwrap_or_default();
    if !grant.contains(action, root) {
        return refuse(
            403,
            "insufficient_scope",
            "transfer path is outside the signed prefix",
        );
    }
    if let Some(entries) = params["manifest"]["entries"].as_array() {
        for entry in entries {
            let path = format!("{root}/{}", entry["path"].as_str().unwrap_or_default());
            if !grant.contains_file(action, &path) {
                return refuse(
                    403,
                    "insufficient_scope",
                    "transfer entry is outside the signed prefix",
                );
            }
        }
    }
    params["wiki_as"] = json!(attribution);
    params["_transfer_binding"] = json!({
        "issuer": grant.issuer,
        "organization_id": grant.organization_id,
        "audience": grant.audience,
        "subject": grant.sub,
    });
    match client::rpc(state_dir, "wiki_transfer", params) {
        Ok(value) => json_ok(value),
        Err(error) => write_err(&error),
    }
}

/// `GET /api/wiki/ls` (and `search`): the daemon's listing or hits, each
/// entry narrowed to the signed ceiling for `wiki.read`.
fn enumerate(
    query: &dyn Fn(&str) -> Option<String>,
    state_dir: &std::path::Path,
    grant: &WikiGrant,
    attribution: &str,
) -> HttpResp {
    let path = query("path").unwrap_or_default();
    if let Some(q) = query("q") {
        let body = match client::rpc(
            state_dir,
            "wiki_search",
            json!({"q": q, "path": path, "wiki_as": attribution}),
        ) {
            Ok(v) => v,
            Err(e) => return write_err(&e),
        };
        let hits: Vec<Value> = body["matches"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|m| {
                m["path"]
                    .as_str()
                    .is_some_and(|p| grant.contains(WikiAction::Enumerate, p))
            })
            .collect();
        return json_ok(json!({"q": body["q"], "base": body["base"], "matches": hits}));
    }
    let body = match client::rpc(
        state_dir,
        "wiki_ls",
        json!({"path": path, "wiki_as": attribution}),
    ) {
        Ok(v) => v,
        Err(e) => return write_err(&e),
    };
    let entries: Vec<Value> = body["entries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| {
            e["path"]
                .as_str()
                .is_some_and(|p| grant.contains(WikiAction::Enumerate, p))
        })
        .collect();
    json_ok(json!({"path": body["path"], "entries": entries}))
}

/// `GET /api/wiki/file`: a page inside the read ceiling. Optional
/// `offset`, `bytes`, and `rev` are the bounded original read. They
/// are validated and forwarded only after the signed prefix check, so
/// a path outside the envelope is still `insufficient_scope` and the
/// daemon does not see the page. A bad integer or an empty revision
/// is refused before that read. Omit all three for the whole-page
/// reply: text JSON, or blob pointer metadata. A blob range returns
/// the daemon's compact JSON (`data_base64` and `range`), never the
/// raw blob and never a line past the native tool bound.
fn read(
    raw_url: &str,
    query: &dyn Fn(&str) -> Option<String>,
    state_dir: &std::path::Path,
    grant: &WikiGrant,
    attribution: &str,
) -> HttpResp {
    let Some(path) = query("path") else {
        return refuse(400, "invalid_request", "wiki file needs ?path=");
    };
    if !grant.contains(WikiAction::Read, path.trim_matches('/')) {
        return refuse(
            403,
            "insufficient_scope",
            "the path is outside this envelope's wiki prefix",
        );
    }
    let mut params = json!({"path": path, "wiki_as": attribution});
    if let Some(raw) = query("offset") {
        match raw.parse::<u64>() {
            Ok(n) => params["offset"] = json!(n),
            Err(_) => {
                return refuse(
                    400,
                    "invalid_request",
                    "wiki file offset must be a non-negative integer",
                );
            }
        }
    }
    if let Some(raw) = query("bytes") {
        match raw.parse::<u64>() {
            Ok(n) => params["bytes"] = json!(n),
            Err(_) => {
                return refuse(
                    400,
                    "invalid_request",
                    "wiki file bytes must be a non-negative integer",
                );
            }
        }
    }
    if let Some(rev) = query("rev") {
        if rev.is_empty() {
            return refuse(
                400,
                "invalid_request",
                "wiki file rev must be a non-empty string",
            );
        }
        params["rev"] = json!(rev);
    }
    match super::wiki::historical_at(raw_url) {
        Ok(Some(at)) => {
            if params.get("rev").is_some() {
                return refuse(
                    400,
                    "invalid_request",
                    "wiki file at is a historical selector and cannot combine with rev",
                );
            }
            params["at"] = json!(at);
        }
        Ok(None) => {}
        Err(message) => return refuse(400, "invalid_request", message),
    }
    let method = if params.get("at").is_some() {
        "wiki_read_revision"
    } else {
        "wiki_read"
    };
    match client::rpc(state_dir, method, params) {
        Ok(v) => bounded_range_json(v),
        Err(e) => write_err(&e),
    }
}

fn bounded_range_json(v: Value) -> HttpResp {
    if v.get("range").is_some() || v.get("data_base64").is_some() {
        match serde_json::to_vec(&v) {
            Ok(body) if body.len().saturating_add(1) < crate::wiki::NATIVE_TOOL_LINE_BOUND => {}
            _ => {
                return refuse(
                    400,
                    "invalid_request",
                    "wiki file range exceeds the native tool line bound",
                );
            }
        }
    }
    json_ok(v)
}

/// `PUT /api/wiki/file` JSON `{path,text,if_rev?}`. The write runs the
/// daemon's `wiki_write` (the existing mutation path) under the
/// envelope's write ceiling. A text write only: no blob fields.
fn write(
    request: &mut Request,
    state_dir: &std::path::Path,
    grant: &WikiGrant,
    attribution: &str,
) -> HttpResp {
    let bytes = match read_body(request, super::write_path::JSON_CAP) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let req: Value = match parse_json(&bytes) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let (Some(path), Some(text)) = (req["path"].as_str(), req["text"].as_str()) else {
        return refuse(400, "invalid_request", "wiki write needs {path,text}");
    };
    if req.as_object().is_some_and(|o| {
        o.keys()
            .any(|k| !matches!(k.as_str(), "path" | "text" | "if_rev"))
    }) {
        return refuse(
            400,
            "invalid_request",
            "wiki write carries an unknown field",
        );
    }
    if !grant.contains(WikiAction::Write, path.trim_matches('/')) {
        return refuse(
            403,
            "insufficient_scope",
            "the path is outside this envelope's wiki write prefix",
        );
    }
    let mut params = json!({"path": path, "text": text, "wiki_as": attribution});
    if let Some(rev) = req["if_rev"].as_str() {
        params["if_rev"] = json!(rev);
    }
    match client::rpc(state_dir, "wiki_write", params) {
        Ok(v) => write_answer(v),
        Err(e) => write_err(&e),
    }
}

/// Validate the logical target before buffering any multipart bytes. The
/// daemon receives this normalized spelling, not another request field.
fn upload_path(path: Option<String>, grant: &WikiGrant) -> Result<String, HttpResp> {
    let path = path.ok_or_else(|| refuse(400, "invalid_request", "wiki upload needs ?path="))?;
    let norm = crate::wiki::upload::logical_upload_path(&path)
        .map_err(|e| refuse(400, "invalid_request", &e.to_string()))?;
    if !grant.contains_file(WikiAction::Write, &norm) {
        return Err(refuse(
            403,
            "insufficient_scope",
            "the path is outside this envelope's wiki write prefix",
        ));
    }
    Ok(norm)
}

/// Verified-envelope uploads accept exactly one file part; multipart fields
/// cannot replace the query target or supply a relay identity.
fn upload(
    request: &mut Request,
    query: &dyn Fn(&str) -> Option<String>,
    state_dir: &std::path::Path,
    pm_dir: &std::path::Path,
    grant: &WikiGrant,
    attribution: &str,
) -> HttpResp {
    let path = match upload_path(query("path"), grant) {
        Ok(path) => path,
        Err(resp) => return resp,
    };
    let bytes = match super::wiki::read_envelope_upload(request, pm_dir) {
        Ok(bytes) => bytes,
        Err(resp) => return resp,
    };
    match super::wiki::store_upload(state_dir, &path, bytes, query("if_rev"), attribution) {
        Ok(v) => write_answer(v),
        Err(resp) => resp,
    }
}

/// A daemon `{conflict: …}` answer (stale `if_rev`) is a refusal, never a
/// success: HTTP 409 with the body, so the remote CLI exits nonzero and
/// never reports a write that did not happen. Anything else is 200.
fn write_answer(v: Value) -> HttpResp {
    if v.get("conflict").is_none() {
        return json_ok(v);
    }
    let mut resp = Response::from_data(serde_json::to_vec(&v).unwrap_or_default())
        .with_status_code(StatusCode(409));
    resp.add_header(tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap());
    resp.add_header(tiny_http::Header::from_bytes("Cache-Control", "no-store").unwrap());
    resp
}

/// The daemon `wiki_as` claim for a verified actor: `operator` for the
/// owner, `user:<h>` for a `users/<h>` member. Anything else has no
/// claim the daemon accepts, so it is `None` and refused.
fn relay_claim(actor: &str) -> Option<String> {
    if actor == "operator" {
        Some("operator".to_string())
    } else {
        actor.strip_prefix("users/").map(|h| format!("user:{h}"))
    }
}

fn json_ok(v: Value) -> HttpResp {
    let mut resp = Response::from_data(serde_json::to_vec(&v).unwrap_or_default())
        .with_status_code(StatusCode(200));
    resp.add_header(tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap());
    resp.add_header(tiny_http::Header::from_bytes("Cache-Control", "no-store").unwrap());
    resp
}

/// Record the envelope's `jti` as spent, until its `exp`. Fails closed.
fn consume_jti(state_dir: &std::path::Path, grant: &WikiGrant) -> bool {
    use crate::cli_actor::Spent;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    let _guard = super::cli_route::SPENT_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut set = Spent::load(state_dir);
    set.consume(&grant.jti, grant.exp, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn verified_upload_grant(actor: &str, prefix: &str) -> WikiGrant {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        use ring::signature::{Ed25519KeyPair, KeyPair};
        let key = Ed25519KeyPair::from_seed_unchecked(&[91; 32]).unwrap();
        let config = board_identity::Config {
            host: "upload.cadencecloud.test".into(),
            issuer: "https://issuer.test".into(),
            company: "fixture".into(),
        };
        let claims = json!({
            "iss":config.issuer,"aud":"https://upload.cadencecloud.test","sub":"fixture",
            "actor":actor,"principal_kind":"user",
            "role_at_issue":if actor == "operator" {"owner"} else {"member"},
            "organization_id":config.company,"scope":["wiki.read","wiki.write"],
            "wiki_prefixes":["global/",prefix],
            "prefix_grants":[{"scope":"wiki.read","prefixes":["global/"]},
                {"scope":"wiki.write","prefixes":[prefix]}],
            "credential_id":"fixture","iat":100,"exp":200,"jti":"fixture"
        });
        let header = json!({"alg":"EdDSA","typ":"agenticos-wiki-actor/1","kid":"fixture"});
        let signed = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let env = format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(key.sign(signed.as_bytes()).as_ref())
        );
        let public_key = board_identity::PublicKey {
            kid: "fixture".into(),
            x: key.public_key().as_ref().try_into().unwrap(),
        };
        envelope::verify(&env, &config, &public_key, 120).unwrap()
    }

    #[test]
    fn uploads_use_verified_owner_and_member_write_ceilings_and_attribution() {
        assert_eq!(action_of(&Method::Post, "upload"), Some(WikiAction::Write));
        assert_eq!(action_of(&Method::Post, "rm"), None);
        for (actor, prefix, claim) in [
            ("operator", "global/covered/", "operator"),
            ("users/u", "users/u/", "user:u"),
        ] {
            let grant = verified_upload_grant(actor, prefix);
            assert_eq!(relay_claim(&grant.actor).as_deref(), Some(claim));
            let directory = prefix.trim_end_matches('/');
            let refusal = match upload_path(Some(directory.into()), &grant) {
                Ok(_) => panic!("the write prefix itself must not be an upload target"),
                Err(refusal) => refusal,
            };
            assert_eq!(refusal.status_code(), StatusCode(403));
            assert!(upload_path(Some(format!("{directory}.extracted.md")), &grant).is_err());
            for suffix in ["source.md", "source.pdf", "nested/source.pdf"] {
                let path = format!("{prefix}{suffix}");
                assert_eq!(
                    upload_path(Some(path.clone()), &grant)
                        .unwrap_or_else(|_| panic!("verified upload target refused")),
                    path
                );
            }
            let nfd = format!("{prefix}cafe\u{301}.md");
            assert_eq!(
                upload_path(Some(nfd), &grant)
                    .unwrap_or_else(|_| panic!("normalized upload target refused")),
                format!("{prefix}caf\u{e9}.md")
            );
            for bad in [
                "global/outside.pdf",
                "global/covered/../outside.pdf",
                "/global/covered/a.pdf",
                "",
                "global/covered/a.blob",
            ] {
                assert!(upload_path(Some(bad.into()), &grant).is_err());
            }
            assert!(upload_path(None, &grant).is_err());
        }
    }

    /// The container's path denial is a real 403 `insufficient_scope` body on a
    /// verified member grant. This is the exact shape the client decoder reads.
    #[test]
    fn a_member_upload_outside_its_signed_prefix_is_403_insufficient_scope() {
        let grant = verified_upload_grant("users/u", "users/u/");
        let refusal = match upload_path(Some("projects/x/a.pdf".into()), &grant) {
            Ok(_) => panic!("a path outside the signed member prefix was accepted"),
            Err(refusal) => refusal,
        };
        assert_eq!(refusal.status_code(), StatusCode(403));
        let mut body = String::new();
        refusal.into_reader().read_to_string(&mut body).unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["ok"], json!(false));
        assert_eq!(parsed["error"]["code"], json!("insufficient_scope"));
        assert!(parsed["error"]["message"].is_string());
    }

    /// The daemon's checked-conflict answer for a stale `if_rev`.
    fn conflict() -> Value {
        json!({"conflict": "if_rev", "current_rev": "fnv1a:829ba77079943a03", "path": "global/a.md"})
    }

    #[test]
    fn a_checked_write_conflict_is_http_409_never_200() {
        // Reproduces CAD-1401 blocker 1: the route answered a conflict as
        // success, so the CLI exited 0 for a write that never happened.
        let resp = write_answer(conflict());
        assert_eq!(resp.status_code(), StatusCode(409));
    }

    #[test]
    fn a_successful_write_stays_http_200() {
        let resp = write_answer(json!({"rev": "fnv1a:0000000000000000"}));
        assert_eq!(resp.status_code(), StatusCode(200));
    }
}
