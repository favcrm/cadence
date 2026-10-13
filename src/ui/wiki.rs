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
    match store_upload(
        state_dir,
        &path,
        bytes,
        query("if_rev").or_else(|| fields.get("if_rev").cloned()),
        wiki_as,
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
            json!({"path":path,"text":text,"if_rev":if_rev,"wiki_as":wiki_as}),
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
        json!({"path": path, "tmp": tmp.path(), "wiki_as": wiki_as,"if_rev": if_rev}),
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
