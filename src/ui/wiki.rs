//! CAD-580: the board's `/api/wiki/*` routes — the daemon's wiki RPCs
//! relayed over the board's own operator connection, with the request's
//! caller riding `wiki_as` exactly as [`super::operator::board_caller`]
//! decided it. The daemon re-derives and re-checks everything — path
//! grammar, the per-prefix allowlist, the upload cap — so the strictest
//! layer wins, and a relayed claim can only ever shrink what the caller
//! reaches (an agent peer names `agent:<itself>`; the operator's session
//! names `operator`; a named board session names `user:<handle>`).
//!
//! Reads go through `board_caller` too: a request the board cannot
//! attribute (no session, no agent tie) is refused before the daemon
//! sees it — the HTTP path is never less strict than the RPC.
//!
//! `/api/wiki/upload` is the one non-JSON write: a multipart body whose
//! single file part streams into `<state>/wiki-uploads/` (the only dir
//! `wiki_put_blob` accepts a tmp from). The part's bytes are capped at
//! the configured upload cap plus a small envelope allowance before
//! they land — the daemon re-checks the file itself against the real
//! cap.

use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response};

use super::{
    err_response, guard_fail, header_value, json_response, operator, parse_json, read_body,
    write_err, HttpResp, ServeOpts, JSON_CAP,
};
use crate::client;
use crate::issue::Pm;
use std::path::Path;

/// The board caller as the daemon's `wiki_as` relay claim.
fn wiki_as(caller: &operator::Caller) -> String {
    match caller {
        operator::Caller::Operator(_) => "operator".to_string(),
        operator::Caller::Agent(alias) => format!("agent:{alias}"),
        operator::Caller::Named(named) => format!("user:{}", named.author),
    }
}

/// Relay `method` to the daemon with `wiki_as` folded into the params.
fn relay(state_dir: &Path, method: &str, mut params: Value, caller: &str) -> HttpResp {
    params["wiki_as"] = json!(caller);
    match client::rpc(state_dir, method, params) {
        Ok(v) => json_response(v),
        Err(e) => write_err(&e),
    }
}

/// A stored compare-and-swap or stale-source answer is HTTP 409. The body
/// is the daemon's conflict object, not a success page.
fn relay_conflict(state_dir: &Path, method: &str, mut params: Value, caller: &str) -> HttpResp {
    params["wiki_as"] = json!(caller);
    match client::rpc(state_dir, method, params) {
        Ok(v) if v.get("conflict").is_some() => {
            let mut resp = Response::from_data(serde_json::to_vec(&v).unwrap_or_default())
                .with_status_code(409);
            resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
            resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
            resp
        }
        Ok(v) => json_response(v),
        Err(e) => write_err(&e),
    }
}

fn board_caller(
    request: &Request,
    state_dir: &Path,
    opts: &ServeOpts,
) -> Result<operator::Caller, HttpResp> {
    operator::board_caller(request, state_dir, opts, false, None)
}

// ---------- GET /api/wiki/{ls,file,search,history} ----------

pub(super) fn read(
    request: &Request,
    tail: &str,
    query: &dyn Fn(&str) -> Option<String>,
    state_dir: &Path,
    pm_dir: &Path,
    opts: &ServeOpts,
) -> HttpResp {
    let caller = match board_caller(request, state_dir, opts) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let wiki_as = wiki_as(&caller);
    match tail {
        "ls" => relay(
            state_dir,
            "wiki_ls",
            json!({"path": query("path").unwrap_or_default()}),
            &wiki_as,
        ),
        "file" => file_response(request, query, state_dir, pm_dir, &wiki_as),
        "search" => match query("q") {
            Some(q) => relay(
                state_dir,
                "wiki_search",
                json!({"q": q, "path": query("path").unwrap_or_default()}),
                &wiki_as,
            ),
            None => err_response(400, "wiki search needs ?q="),
        },
        "history" => {
            let mut params = json!({"path": query("path").unwrap_or_default()});
            if let Some(n) = query("limit").and_then(|s| s.parse::<u64>().ok()) {
                params["limit"] = json!(n);
            }
            relay(state_dir, "wiki_history", params, &wiki_as)
        }
        "collection" | "corrections" => {
            let mut params = json!({"path": query("path").unwrap_or_default()});
            if let Err(resp) =
                operator::relay_owner_proof(&mut params, operator::public_relay(request, opts))
            {
                return resp;
            }
            let method = if tail == "collection" {
                "wiki_collection_get"
            } else {
                "wiki_corrections_list"
            };
            relay(state_dir, method, params, &wiki_as)
        }
        _ => err_response(404, "no such wiki route"),
    }
}

/// Judge raw `at` pairs before the decoded query helper. That helper drops a
/// value percent-decode cannot finish and keeps only the first `at`, so a
/// bad or repeated selector has to be refused here. The name is compared
/// after percent-decoding, so `%61t` and `a%74` are the selector rather than
/// an unknown parameter that quietly reads the live page.
pub(super) fn historical_at(raw_url: &str) -> std::result::Result<Option<String>, &'static str> {
    let raw_query = raw_url
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or("");
    let mut selector = None;
    for pair in raw_query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, raw) = pair.split_once('=').unwrap_or((pair, ""));
        // The name is judged decoded as well: a selector spelled `%61t` asks
        // for a pinned revision exactly as `at` does, and must not fall
        // through to a live read as if the parameter were something else. A
        // key that cannot be decoded cannot be a spelling of `at`.
        let Some(key) = super::pct_decode(key) else {
            continue;
        };
        if key != "at" {
            continue;
        }
        if selector.is_some() {
            return Err("wiki file at is repeated");
        }
        let Some(decoded) = super::pct_decode(raw) else {
            return Err("wiki file at is malformed");
        };
        if decoded.is_empty() {
            return Err("wiki file at must be a non-empty revision");
        }
        selector = Some(decoded);
    }
    Ok(selector)
}

/// `GET /api/wiki/file?path=` — text comes back as the daemon's JSON.
/// Optional `offset`, `bytes`, and `rev` are the same bounded original
/// read as `wiki_read`: absent, the reply is the whole page; present,
/// the daemon returns one range or refuses with no body. A blob range
/// query returns that bounded JSON (`data_base64` plus the pointer),
/// not the file bytes. With no range query, a blob page streams
/// `.blobs/<sha256>`: byte ranges for seeking, `attachment` +
/// octet-stream for everything but the small inline allowlist
/// (image/video/pdf), and a sandboxed CSP either way. SVG and HTML
/// are NEVER inline — a rendered wiki attachment must not drive the
/// board's write API.
fn file_response(
    request: &Request,
    query: &dyn Fn(&str) -> Option<String>,
    state_dir: &Path,
    pm_dir: &Path,
    wiki_as: &str,
) -> HttpResp {
    let Some(path) = query("path") else {
        return err_response(400, "wiki file needs ?path=");
    };
    let mut params = json!({"path": path, "wiki_as": wiki_as});
    if let Some(raw) = query("offset") {
        match raw.parse::<u64>() {
            Ok(n) => params["offset"] = json!(n),
            Err(_) => {
                return err_response(400, "wiki file offset must be a non-negative integer");
            }
        }
    }
    if let Some(raw) = query("bytes") {
        match raw.parse::<u64>() {
            Ok(n) => params["bytes"] = json!(n),
            Err(_) => {
                return err_response(400, "wiki file bytes must be a non-negative integer");
            }
        }
    }
    if let Some(rev) = query("rev") {
        if rev.is_empty() {
            return err_response(400, "wiki file rev must be a non-empty string");
        }
        params["rev"] = json!(rev);
    }
    match historical_at(request.url()) {
        Ok(Some(at)) => {
            if params.get("rev").is_some() {
                return err_response(
                    400,
                    "wiki file at is a historical selector and cannot combine with rev",
                );
            }
            params["at"] = json!(at);
        }
        Ok(None) => {}
        Err(message) => return err_response(400, message),
    }
    let historical = params.get("at").is_some();
    let ranged = params.get("offset").is_some()
        || params.get("bytes").is_some()
        || params.get("rev").is_some();
    let method = if historical {
        "wiki_read_revision"
    } else {
        "wiki_read"
    };
    let page = match client::rpc(state_dir, method, params) {
        Ok(v) => v,
        Err(e) => return write_err(&e),
    };
    // A historical read that asks for JSON returns the page document,
    // including a blob pointer. Raw bytes stay the default for a live
    // attachment and for a historical blob that did not ask for JSON.
    let wants_json = header_value(request, "Accept")
        .is_some_and(|value| value.to_ascii_lowercase().contains("application/json"));
    if historical && wants_json {
        return json_response(page);
    }
    match page["kind"].as_str() {
        Some("text") | Some("dir") => return json_response(page),
        Some("blob") if ranged => return compact_wiki_json(page),
        Some("blob") => {}
        _ => return err_response(404, "no such wiki page"),
    }
    let sha = page["sha256"].as_str().unwrap_or_default();
    let mime = page["mime"].as_str().unwrap_or("application/octet-stream");
    let name = page["name"].as_str().unwrap_or("attachment");
    let blob = match Pm::at(pm_dir)
        .and_then(|pm| crate::wiki::vault_dir(&pm))
        .map(|vault| crate::wiki::blobs_dir(&vault).join(sha))
    {
        Ok(p) => p,
        Err(e) => return err_response(503, &e.to_string()),
    };
    // sha256 hex only — never a path the caller supplied.
    if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return err_response(500, "blob pointer carries a bad sha256");
    }
    let data = match std::fs::read(&blob) {
        Ok(d) => d,
        Err(_) => return err_response(404, "blob bytes are gone"),
    };

    // Range: bytes=a-b | bytes=a- | bytes=-n (a single range; anything
    // else is ignored and the whole file answers).
    let total = data.len() as u64;
    let mut status = 200;
    let mut span: (u64, u64) = (0, total); // [start, end)
    if let Some(range) = header_value(request, "Range") {
        if let Some(spec) = range.trim().strip_prefix("bytes=") {
            let mut it = spec.splitn(2, '-');
            let (a, b) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
            let parsed = if a.is_empty() {
                b.parse::<u64>()
                    .ok()
                    .map(|n| (total.saturating_sub(n), total))
            } else {
                a.parse::<u64>().ok().map(|start| {
                    let end = b
                        .parse::<u64>()
                        .ok()
                        .map(|e| e.saturating_add(1).min(total))
                        .unwrap_or(total);
                    (start, end)
                })
            };
            match parsed {
                Some((start, end)) if start < end && start < total => {
                    status = 206;
                    span = (start, end.min(total));
                }
                Some(_) => {
                    let mut resp = Response::from_data(Vec::<u8>::new()).with_status_code(416);
                    resp.add_header(
                        Header::from_bytes("Content-Range", format!("bytes */{total}")).unwrap(),
                    );
                    return resp;
                }
                None => {} // malformed — answer the whole file
            }
        }
    }
    let body = data[span.0 as usize..span.1 as usize].to_vec();

    // The inline allowlist: images (never svg), video, pdf. Everything
    // else — and always svg/html/octet — is an attachment download.
    let inline = matches!(mime.split('/').next(), Some("image") | Some("video"))
        && mime != "image/svg+xml"
        || mime == "application/pdf";
    let mut resp = Response::from_data(body).with_status_code(status);
    let ct = if inline {
        mime
    } else {
        "application/octet-stream"
    };
    resp.add_header(Header::from_bytes("Content-Type", ct).unwrap());
    resp.add_header(
        Header::from_bytes("Content-Security-Policy", "sandbox; default-src 'none'").unwrap(),
    );
    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    resp.add_header(Header::from_bytes("Accept-Ranges", "bytes").unwrap());
    if status == 206 {
        resp.add_header(
            Header::from_bytes(
                "Content-Range",
                format!("bytes {}-{}/{}", span.0, span.1 - 1, total),
            )
            .unwrap(),
        );
    }
    if !inline {
        let safe_name: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        resp.add_header(
            Header::from_bytes(
                "Content-Disposition",
                format!("attachment; filename=\"{safe_name}\""),
            )
            .unwrap(),
        );
    }
    resp
}

fn compact_wiki_json(page: Value) -> HttpResp {
    let Ok(body) = serde_json::to_vec(&page) else {
        return err_response(500, "wiki file range could not be encoded");
    };
    if body.len().saturating_add(1) >= crate::wiki::NATIVE_TOOL_LINE_BOUND {
        return err_response(400, "wiki file range exceeds the native tool line bound");
    }
    let mut resp = Response::from_data(body).with_status_code(200);
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    resp
}

// ---------- PUT /api/wiki/file, POST /api/wiki/{upload,mkdir,mv,rm} ----------

pub(super) fn write(
    request: &mut Request,
    method: &Method,
    tail: &str,
    caller: &operator::Caller,
    query: &dyn Fn(&str) -> Option<String>,
    dirs: (&Path, &Path),
    relay_proof: operator::PublicRelay,
) -> HttpResp {
    let (state_dir, pm_dir) = dirs;
    let wiki_as = wiki_as(caller);
    // Attach the server-held session to writes and caller-owned transfer reads.
    // Neither a caller-selected identity nor an absent public proof is relayed.
    let proof = match relay_proof {
        operator::PublicRelay::Missing => {
            return err_response(401, "board session required for this request");
        }
        operator::PublicRelay::NotPublic => None,
        operator::PublicRelay::Proven(p) => Some(p.token().to_string()),
    };
    let with_proof = |mut params: Value| {
        if let Some(token) = &proof {
            params["public_session"] = Value::String(token.clone());
        }
        params
    };
    match (tail, method) {
        ("transfer", Method::Put) | ("job_status", Method::Post) => {
            let body = match read_body(request, JSON_CAP) {
                Ok(body) => body,
                Err(resp) => return resp,
            };
            let body: Value = match parse_json(&body) {
                Ok(body) => body,
                Err(resp) => return resp,
            };
            match transfer_params(&body, tail == "transfer") {
                Ok(params) => relay(state_dir, "wiki_transfer", with_proof(params), &wiki_as),
                Err(resp) => resp,
            }
        }
        ("collection", Method::Put) => {
            let body = match read_body(request, JSON_CAP) {
                Ok(body) => body,
                Err(resp) => return resp,
            };
            let req: Value = match parse_json(&body) {
                Ok(req) => req,
                Err(resp) => return resp,
            };
            let Some(body) = req.as_object() else {
                return err_response(400, "wiki collection needs {path}");
            };
            if body
                .keys()
                .any(|key| !matches!(key.as_str(), "path" | "if_rev" | "metadata"))
            {
                return err_response(400, "wiki collection has unsupported fields");
            }
            let Some(path) = req["path"].as_str() else {
                return err_response(400, "wiki collection needs {path}");
            };
            let Some(if_rev) = req["if_rev"].as_str() else {
                return err_response(400, "wiki collection needs {if_rev}");
            };
            let Some(metadata) = req.get("metadata") else {
                return err_response(400, "wiki collection needs {metadata}");
            };
            relay_conflict(
                state_dir,
                "wiki_collection_put",
                with_proof(json!({"path": path, "if_rev": if_rev, "metadata": metadata})),
                &wiki_as,
            )
        }
        ("corrections" | "corrections/review", Method::Post) => {
            let body = match read_body(request, JSON_CAP) {
                Ok(body) => body,
                Err(resp) => return resp,
            };
            let mut req: Value = match parse_json(&body) {
                Ok(req) => req,
                Err(resp) => return resp,
            };
            let Some(path) = req.as_object_mut().and_then(|body| body.remove("path")) else {
                return err_response(400, "wiki correction needs a path");
            };
            // Keep user-reported data separate from the trusted session envelope.
            // Move the report rather than copying the potentially large answer.
            let params = Value::Object(serde_json::Map::from_iter([
                ("path".to_string(), path),
                ("correction".to_string(), req),
            ]));
            let method = if tail == "corrections" {
                "wiki_corrections_submit"
            } else {
                "wiki_corrections_review"
            };
            relay_conflict(state_dir, method, with_proof(params), &wiki_as)
        }
        ("file", Method::Put) => {
            let body = match read_body(request, JSON_CAP) {
                Ok(b) => b,
                Err(resp) => return resp,
            };
            let req: Value = match parse_json(&body) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let Some(path) = req["path"].as_str() else {
                return err_response(400, "wiki file write needs {path}");
            };
            let Some(text) = req["text"].as_str() else {
                return err_response(400, "wiki file write needs {text}");
            };
            let mut params = json!({"path": path, "text": text});
            if let Some(rev) = req["if_rev"].as_str() {
                params["if_rev"] = json!(rev);
            }
            relay(state_dir, "wiki_write", with_proof(params), &wiki_as)
        }
        ("mkdir", Method::Post) | ("rm", Method::Post) => {
            let body = match read_body(request, JSON_CAP) {
                Ok(b) => b,
                Err(resp) => return resp,
            };
            let req: Value = match parse_json(&body) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let Some(path) = req["path"].as_str() else {
                return err_response(400, format!("wiki {tail} needs {{path}}").as_str());
            };
            relay(
                state_dir,
                if tail == "mkdir" {
                    "wiki_mkdir"
                } else {
                    "wiki_rm"
                },
                with_proof(json!({"path": path})),
                &wiki_as,
            )
        }
        ("mv", Method::Post) => {
            let body = match read_body(request, JSON_CAP) {
                Ok(b) => b,
                Err(resp) => return resp,
            };
            let req: Value = match parse_json(&body) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            match (req["from"].as_str(), req["to"].as_str()) {
                (Some(from), Some(to)) => relay(
                    state_dir,
                    "wiki_mv",
                    with_proof(json!({"from": from, "to": to})),
                    &wiki_as,
                ),
                _ => err_response(400, "wiki mv needs {from,to}"),
            }
        }
        ("upload", Method::Post) => upload(request, query, state_dir, pm_dir, &wiki_as, proof),
        (
            "file" | "upload" | "mkdir" | "mv" | "rm" | "transfer" | "job_status" | "collection"
            | "corrections" | "corrections/review",
            _,
        ) => err_response(405, "method not allowed"),
        _ => err_response(404, "no such wiki write route"),
    }
}

/// Parse the bounded transfer transport, never a caller-selected RPC method or
/// authority relay. Both browser and signed-envelope adapters use this check.
pub(super) fn transfer_params(body: &Value, writing: bool) -> Result<Value, HttpResp> {
    let Some(outer) = body.as_object() else {
        return Err(err_response(400, "wiki transfer needs an object"));
    };
    if outer
        .keys()
        .any(|key| !matches!(key.as_str(), "path" | "transfer"))
    {
        return Err(err_response(400, "wiki transfer has unsupported fields"));
    }
    let Some(path) = body["path"].as_str() else {
        return Err(err_response(400, "wiki transfer needs a path"));
    };
    if path.is_empty()
        || path.len() > 512
        || !crate::wiki::normalize(path).is_ok_and(|normalized| normalized == path)
    {
        return Err(err_response(400, "wiki transfer path must be canonical"));
    }
    let Some(transfer) = body["transfer"].as_object() else {
        return Err(err_response(
            400,
            "wiki transfer needs an explicit operation",
        ));
    };
    if transfer.keys().any(|key| {
        !matches!(
            key.as_str(),
            "op" | "manifest"
                | "job_id"
                | "root"
                | "entry_id"
                | "offset"
                | "data_base64"
                | "limit"
                | "path"
                | "bytes"
                | "rev"
                | "at"
        )
    }) {
        return Err(err_response(
            400,
            "wiki transfer operation has unsupported fields",
        ));
    }
    let op = transfer
        .get("op")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !(if writing {
        matches!(op, "begin" | "chunk" | "apply" | "finish")
    } else {
        matches!(op, "status" | "export")
    }) {
        return Err(err_response(
            400,
            "wiki transfer operation does not match this route",
        ));
    }
    for key in ["root", "path"] {
        if transfer
            .get(key)
            .is_some_and(|value| value.as_str() != Some(path))
        {
            return Err(err_response(
                400,
                "wiki transfer target does not match the route path",
            ));
        }
    }
    if op == "begin" {
        let manifest = &body["transfer"]["manifest"];
        if manifest["root"].as_str() != Some(path) {
            return Err(err_response(
                400,
                "wiki transfer manifest root does not match the route",
            ));
        }
        let Some(entries) = manifest["entries"]
            .as_array()
            .filter(|entries| entries.len() <= 256)
        else {
            return Err(err_response(400, "wiki transfer needs a bounded manifest"));
        };
        for entry in entries {
            let Some(relative) = entry["path"].as_str() else {
                return Err(err_response(
                    400,
                    "wiki transfer entry needs a relative path",
                ));
            };
            if relative.is_empty()
                || relative.len() > 512
                || !crate::wiki::normalize(relative).is_ok_and(|normalized| normalized == relative)
            {
                return Err(err_response(
                    400,
                    "wiki transfer entry path must be canonical",
                ));
            }
        }
    }
    let mut params = Value::Object(transfer.clone());
    params["root"] = json!(path);
    if op == "export" {
        params["path"] = json!(path);
    }
    Ok(params)
}

/// `POST /api/wiki/upload?path=` — a multipart body whose single file
/// part (`file`; `path` may also ride as a field) streams into
/// `<state>/wiki-uploads/upload-*`; the daemon's `wiki_put_blob`
/// re-hashes, caps, sniffs and lands it. Buffering stops at the
/// configured cap plus a small envelope allowance.
fn upload(
    request: &mut Request,
    query: &dyn Fn(&str) -> Option<String>,
    state_dir: &Path,
    pm_dir: &Path,
    wiki_as: &str,
    proof: Option<String>,
) -> HttpResp {
    let ct = header_value(request, "Content-Type").unwrap_or_default();
    let Some(boundary) = ct
        .trim()
        .strip_prefix("multipart/form-data; boundary=")
        .map(str::trim)
        .map(|b| b.trim_matches('"'))
    else {
        return guard_fail(
            "content_type",
            "upload needs a multipart/form-data boundary",
        );
    };
    if boundary.is_empty() || boundary.len() > 100 {
        return err_response(400, "bad multipart boundary");
    }
    let cap = Pm::at(pm_dir)
        .map(|pm| pm.config.wiki.max_upload_bytes)
        .unwrap_or(100 * 1024 * 1024)
        // The multipart envelope rides above the file cap.
        .saturating_add(64 * 1024);
    let body = match read_body(request, cap) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let (fields, file) = match super::parse_multipart(&body, boundary) {
        Ok(v) => v,
        Err(why) => return err_response(400, &why),
    };
    let Some(path) = query("path").or_else(|| fields.get("path").cloned()) else {
        return err_response(400, "wiki upload needs ?path= or a 'path' field");
    };
    let Some(bytes) = file else {
        return err_response(400, "wiki upload needs a 'file' part");
    };
    match store_upload_with_proof(
        state_dir,
        &path,
        bytes,
        query("if_rev").or_else(|| fields.get("if_rev").cloned()),
        wiki_as,
        proof.as_deref(),
    ) {
        Ok(v) => json_response(v),
        Err(resp) => resp,
    }
}

/// Shared landing path. Callers own authentication, target validation and
/// multipart policy; neither a filename nor a body field supplies authority.
pub(super) fn store_upload(
    state_dir: &Path,
    path: &str,
    bytes: Vec<u8>,
    if_rev: Option<String>,
    wiki_as: &str,
) -> Result<Value, HttpResp> {
    store_upload_with_proof(state_dir, path, bytes, if_rev, wiki_as, None)
}

/// `store_upload` with the browser's trusted public-session relay token.
/// `proof` only ever comes from the server's own `PublicRelay` decision
/// upstream — never from the request — so a `None` caller cannot name a
/// session. Both RPC branches forward it as `public_session` so the daemon
/// re-checks membership and audits the verified actor before any effect.
fn store_upload_with_proof(
    state_dir: &Path,
    path: &str,
    bytes: Vec<u8>,
    if_rev: Option<String>,
    wiki_as: &str,
    proof: Option<&str>,
) -> Result<Value, HttpResp> {
    // Fold the server-held bearer into RPC params only when one was proven.
    let with_proof = |mut params: Value| {
        if let Some(token) = proof {
            params["public_session"] = Value::String(token.to_string());
        }
        params
    };
    if bytes.is_empty() {
        return Err(err_response(400, "wiki upload: the file part is empty"));
    }
    // Text sources are first-class Git pages. Sending them through the blob
    // route would make an uploaded Markdown or TXT file invisible to search.
    let is_text = [".md", ".markdown", ".txt"]
        .iter()
        .any(|ext| path.to_ascii_lowercase().ends_with(ext));
    if is_text {
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => return Err(err_response(400, "text upload is not UTF-8")),
        };
        return client::rpc(
            state_dir,
            "wiki_write",
            with_proof(json!({"path":path,"text":text,"if_rev":if_rev,"wiki_as":wiki_as})),
        )
        .map_err(|e| write_err(&e));
    }
    let uploads = state_dir.join(crate::wiki::UPLOAD_DIR);
    std::fs::create_dir_all(&uploads)
        .map_err(|e| err_response(500, &format!("upload staging failed: {e}")))?;
    // RAII removes our private staging file on every outcome, including a
    // conflict or a failed write. The daemon may already have consumed it.
    let mut tmp = tempfile::Builder::new()
        .prefix("upload-")
        .tempfile_in(&uploads)
        .map_err(|e| err_response(500, &format!("upload staging failed: {e}")))?;
    std::io::Write::write_all(&mut tmp, &bytes)
        .map_err(|e| err_response(500, &format!("upload staging failed: {e}")))?;
    client::rpc(
        state_dir,
        "wiki_put_blob",
        with_proof(json!({"path": path, "tmp": tmp.path(), "wiki_as": wiki_as,"if_rev": if_rev})),
    )
    .map_err(|e| write_err(&e))
}

/// The envelope proxy contract is one file with its target in the query.
/// Keep the browser's field-capable parser above unchanged.
pub(super) fn read_envelope_upload(
    request: &mut Request,
    pm_dir: &Path,
) -> Result<Vec<u8>, HttpResp> {
    let ct = header_value(request, "Content-Type").unwrap_or_default();
    let boundary = ct
        .trim()
        .strip_prefix("multipart/form-data; boundary=")
        .map(str::trim)
        .and_then(|b| {
            if b.starts_with('"') {
                b.strip_prefix('"')?.strip_suffix('"')
            } else {
                Some(b)
            }
        })
        .filter(|b| {
            !b.is_empty()
                && b.len() <= 100
                && b.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"'()+_,-./:=?".contains(&c))
        })
        .ok_or_else(|| err_response(400, "upload needs a valid multipart/form-data boundary"))?;
    let file_cap = Pm::at(pm_dir)
        .map(|pm| pm.config.wiki.max_upload_bytes)
        .map_err(|e| err_response(500, &format!("upload configuration unavailable: {e}")))?
        .min(crate::wiki::upload::UPLOAD_CAP_BYTES);
    // Same 64 KiB envelope allowance as the browser, below the proxy's
    // 16 MiB body ceiling. Check the actual file cap separately as well.
    let body = read_body(request, file_cap.saturating_add(64 * 1024))?;
    let bytes = single_file_part(&body, boundary).map_err(|why| err_response(400, why))?;
    if bytes.len() as u64 > file_cap {
        return Err(err_response(413, "wiki upload file is over the upload cap"));
    }
    Ok(bytes)
}

/// Byte-safe and closed: exactly one `file` part, no fields, duplicate
/// headers/parameters, second part, missing terminator or trailing body.
fn single_file_part(body: &[u8], boundary: &str) -> Result<Vec<u8>, &'static str> {
    let start = format!("--{boundary}\r\n");
    let rest = body
        .strip_prefix(start.as_bytes())
        .ok_or("bad multipart start")?;
    let header_end = rest
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("multipart part has no header/body split")?;
    if header_end > 64 * 1024 {
        return Err("multipart headers are too large");
    }
    let headers = std::str::from_utf8(&rest[..header_end]).map_err(|_| "bad multipart headers")?;
    let mut disposition = None;
    let mut content_type = false;
    for line in headers.split("\r\n") {
        let (name, value) = line.split_once(':').ok_or("bad multipart header")?;
        if value.chars().any(|c| c.is_control()) {
            return Err("bad multipart header value");
        }
        match name.to_ascii_lowercase().as_str() {
            "content-disposition" if disposition.is_none() => disposition = Some(value.trim()),
            "content-type" if !content_type && !value.trim().is_empty() => content_type = true,
            _ => return Err("unknown or duplicate multipart header"),
        }
    }
    let mut parts = disposition
        .ok_or("missing multipart disposition")?
        .split(';');
    if parts.next().map(str::trim) != Some("form-data") {
        return Err("bad multipart disposition");
    }
    let mut name = None;
    let mut filename = false;
    for parameter in parts {
        let (key, value) = parameter
            .trim()
            .split_once('=')
            .ok_or("bad multipart parameter")?;
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .filter(|v| !v.contains('"'))
            .ok_or("bad multipart parameter value")?;
        match key {
            "name" if name.is_none() => name = Some(value),
            "filename" if !filename => filename = true,
            _ => return Err("unknown or duplicate multipart parameter"),
        }
    }
    if name != Some("file") {
        return Err("upload accepts only a single 'file' part");
    }
    let payload = &rest[header_end + 4..];
    let marker = format!("\r\n--{boundary}");
    let end = payload
        .windows(marker.len())
        .position(|w| w == marker.as_bytes())
        .ok_or("multipart file never terminates")?;
    let trailer = &payload[end + marker.len()..];
    if trailer != b"--" && trailer != b"--\r\n" {
        return Err("upload accepts exactly one file and no trailing body");
    }
    if end == 0 {
        return Err("wiki upload: the file part is empty");
    }
    Ok(payload[..end].to_vec())
}

#[cfg(test)]
mod upload_tests {
    use super::*;
    use crate::wiki::upload::build_multipart;

    #[test]
    fn single_file_preserves_text_and_binary_bytes() {
        for bytes in [
            b"# Notes\n\nUploaded text\n".as_slice(),
            b"\x00\xff\x80binary\r\n".as_slice(),
        ] {
            let body = build_multipart("fixture", &[], "source", bytes);
            assert_eq!(single_file_part(&body, "fixture").unwrap(), bytes);
        }
    }

    #[test]
    fn fields_extra_files_and_malformed_parts_fail_closed() {
        let good = build_multipart("fixture", &[], "source", b"one");
        let mut multiple = good[..good.len() - b"--fixture--\r\n".len()].to_vec();
        multiple.extend_from_slice(&good);
        let mut trailing = good.clone();
        trailing.extend_from_slice(b"ignored");
        let duplicate = String::from_utf8(good.clone())
            .unwrap()
            .replace("name=\"file\"", "name=\"file\"; name=\"other\"")
            .into_bytes();
        let unknown_file = String::from_utf8(good.clone())
            .unwrap()
            .replace("name=\"file\"", "name=\"other\"")
            .into_bytes();
        for body in [
            build_multipart(
                "fixture",
                &[("path", "global/elsewhere.pdf")],
                "source",
                b"one",
            ),
            build_multipart("fixture", &[("wiki_as", "operator")], "source", b"one"),
            build_multipart("fixture", &[], "source", b""),
            good[..good.len() - 5].to_vec(),
            multiple,
            trailing,
            duplicate,
            unknown_file,
        ] {
            assert!(single_file_part(&body, "fixture").is_err());
        }
    }

    #[test]
    fn browser_field_parser_is_still_field_capable() {
        let body = build_multipart(
            "fixture",
            &[("path", "users/u/file.txt"), ("if_rev", "none")],
            "source",
            b"text",
        );
        let (fields, file) = super::super::parse_multipart(&body, "fixture").unwrap();
        assert_eq!(fields["path"], "users/u/file.txt");
        assert_eq!(fields["if_rev"], "none");
        assert_eq!(file.unwrap(), b"text");
    }

    #[test]
    fn invalid_text_and_empty_uploads_never_stage() {
        let root = tempfile::tempdir().unwrap();
        for bytes in [Vec::new(), vec![0xff]] {
            assert!(store_upload(root.path(), "users/u/source.md", bytes, None, "user:u").is_err());
            assert!(!root.path().join(crate::wiki::UPLOAD_DIR).exists());
        }
    }
}

#[cfg(test)]
mod selector_tests {
    use super::historical_at;

    /// The selector is read from the raw query, so a present `at` that cannot
    /// be decoded, is empty, or appears twice is refused instead of quietly
    /// reading the live page.
    #[test]
    fn a_malformed_empty_or_repeated_at_is_refused() {
        assert_eq!(historical_at("/api/wiki/file?path=a.md"), Ok(None));
        assert_eq!(
            historical_at("/api/wiki/file?path=a.md&at=fnv1a%3A0123456789abcdef"),
            Ok(Some("fnv1a:0123456789abcdef".to_string()))
        );
        assert_eq!(
            historical_at("/api/wiki/file?path=a.md&at="),
            Err("wiki file at must be a non-empty revision")
        );
        for malformed in ["%ZZ", "%FF"] {
            assert_eq!(
                historical_at(&format!("/api/wiki/file?path=a.md&at={malformed}")),
                Err("wiki file at is malformed")
            );
        }
        assert_eq!(
            historical_at("/api/wiki/file?at=a&at=0123456789abcdef0123456789abcdef01234567"),
            Err("wiki file at is repeated")
        );
    }

    /// An encoded spelling of the selector name is the selector. Compared
    /// literally, `%61t=…` was an unknown parameter, so the request fell
    /// through and read the live page instead of the revision asked for.
    #[test]
    fn an_encoded_at_spelling_is_the_selector_not_a_live_read() {
        for spelling in ["at", "%61t", "a%74", "%61%74"] {
            assert_eq!(
                historical_at(&format!(
                    "/api/wiki/file?path=a.md&{spelling}=fnv1a%3A0123456789abcdef"
                )),
                Ok(Some("fnv1a:0123456789abcdef".to_string())),
                "{spelling} names the selector"
            );
        }
        assert_eq!(
            historical_at(
                "/api/wiki/file?path=a.md&at=fnv1a%3A0123456789abcdef&%61t=fnv1a%3A0123456789abcdef"
            ),
            Err("wiki file at is repeated")
        );
        assert_eq!(
            historical_at("/api/wiki/file?path=a.md&%61t="),
            Err("wiki file at must be a non-empty revision")
        );
        assert_eq!(
            historical_at("/api/wiki/file?path=a.md&%61t=%ZZ"),
            Err("wiki file at is malformed")
        );
    }
}
