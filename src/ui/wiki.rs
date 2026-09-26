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

fn board_caller(
    request: &Request,
    state_dir: &Path,
    opts: &ServeOpts,
) -> Result<operator::Caller, HttpResp> {
    operator::board_caller(request, state_dir, opts, false)
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
        _ => err_response(404, "no such wiki route"),
    }
}

/// `GET /api/wiki/file?path=` — text comes back as the daemon's JSON;
/// a blob page streams `.blobs/<sha256>` itself: byte ranges for
/// seeking, `attachment` + octet-stream for everything but the small
/// inline allowlist (image/video/pdf), and a sandboxed CSP either way.
/// SVG and HTML are NEVER inline — a rendered wiki attachment must not
/// drive the board's write API.
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
    let page = match client::rpc(
        state_dir,
        "wiki_read",
        json!({"path": path, "wiki_as": wiki_as}),
    ) {
        Ok(v) => v,
        Err(e) => return write_err(&e),
    };
    match page["kind"].as_str() {
        Some("text") | Some("dir") => return json_response(page),
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

// ---------- PUT /api/wiki/file, POST /api/wiki/{upload,mkdir,mv,rm} ----------

pub(super) fn write(
    request: &mut Request,
    method: &Method,
    tail: &str,
    caller: &operator::Caller,
    query: &dyn Fn(&str) -> Option<String>,
    state_dir: &Path,
    pm_dir: &Path,
) -> HttpResp {
    let wiki_as = wiki_as(caller);
    match (tail, method) {
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
            relay(state_dir, "wiki_write", params, &wiki_as)
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
                json!({"path": path}),
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
                    json!({"from": from, "to": to}),
                    &wiki_as,
                ),
                _ => err_response(400, "wiki mv needs {from,to}"),
            }
        }
        ("upload", Method::Post) => upload(request, query, state_dir, pm_dir, &wiki_as),
        ("file" | "upload" | "mkdir" | "mv" | "rm", _) => err_response(405, "method not allowed"),
        _ => err_response(404, "no such wiki write route"),
    }
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
    let (fields, file) = match parse_multipart(&body, boundary) {
        Ok(v) => v,
        Err(why) => return err_response(400, &why),
    };
    let Some(path) = query("path").or_else(|| fields.get("path").cloned()) else {
        return err_response(400, "wiki upload needs ?path= or a 'path' field");
    };
    let Some(bytes) = file else {
        return err_response(400, "wiki upload needs a 'file' part");
    };
    if bytes.is_empty() {
        return err_response(400, "wiki upload: the file part is empty");
    }
    let uploads = state_dir.join(crate::wiki::UPLOAD_DIR);
    if let Err(e) = std::fs::create_dir_all(&uploads) {
        return err_response(500, &format!("upload staging failed: {e}"));
    }
    let tmp = uploads.join(format!("upload-{}", uuid::Uuid::new_v4().simple()));
    if let Err(e) = std::fs::write(&tmp, &bytes) {
        return err_response(500, &format!("upload staging failed: {e}"));
    }
    match client::rpc(
        state_dir,
        "wiki_put_blob",
        json!({"path": path, "tmp": tmp, "wiki_as": wiki_as,
               "if_rev": query("if_rev").or_else(|| fields.get("if_rev").cloned())}),
    ) {
        Ok(v) => json_response(v),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            write_err(&e)
        }
    }
}

/// `needle`'s first offset in `hay`.
fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

type Multipart = (std::collections::BTreeMap<String, String>, Option<Vec<u8>>);

/// The simplest multipart reader that is still strict — and byte-safe:
/// file payloads never pass through a UTF-8 decode. The body must be
/// `--b\r\n<part-headers>\r\n\r\n<payload>\r\n--b\r\n…\r\n--b--`.
/// Named fields land in `fields`; the FIRST `file` part's raw payload
/// is the upload. A payload containing `\r\n--b` truncates early —
/// the boundary is the client's own random marker, as the format
/// intends.
fn parse_multipart(body: &[u8], boundary: &str) -> Result<Multipart, String> {
    let delim = format!("--{boundary}").into_bytes();
    let crlf_delim = {
        let mut v = b"\r\n".to_vec();
        v.extend_from_slice(&delim);
        v
    };
    let mut fields = std::collections::BTreeMap::new();
    let mut file = None;
    let mut pos = 0;
    loop {
        if !body[pos..].starts_with(&delim) {
            return Err("multipart part does not start with the boundary".to_string());
        }
        pos += delim.len();
        if body[pos..].starts_with(b"--") {
            break; // the final delimiter
        }
        if body[pos..].starts_with(b"\r\n") {
            pos += 2;
        } else {
            return Err("multipart boundary is not followed by a part".to_string());
        }
        let rest = &body[pos..];
        let Some(hdr_len) = find_sub(rest, b"\r\n\r\n") else {
            return Err("multipart part with no header/body split".to_string());
        };
        let headers = String::from_utf8_lossy(&rest[..hdr_len]);
        let content_start = pos + hdr_len + 4;
        let Some(rel_end) = find_sub(&body[content_start..], &crlf_delim) else {
            return Err("multipart part never terminates".to_string());
        };
        let payload = &body[content_start..content_start + rel_end];
        let mut name = None;
        for line in headers.lines() {
            if line
                .to_ascii_lowercase()
                .starts_with("content-disposition:")
            {
                for seg in line.split(';') {
                    let seg = seg.trim();
                    if let Some(v) = seg.strip_prefix("name=") {
                        name = Some(v.trim_matches('"').to_string());
                    }
                }
            }
        }
        match name.as_deref() {
            Some("file") => {
                if file.is_none() {
                    file = Some(payload.to_vec());
                }
            }
            Some(n) => {
                fields.insert(n.to_string(), String::from_utf8_lossy(payload).to_string());
            }
            None => {}
        }
        pos = content_start + rel_end + 2;
    }
    Ok((fields, file))
}
