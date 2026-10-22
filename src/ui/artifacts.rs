//! CAD-1726: the board's read of a canvas artifact —
//! `GET /api/artifacts/:id/:version?conversation=<id>` (bytes) and
//! `GET /api/artifacts/:id?conversation=<id>` (the version list).
//!
//! Both rows are `OperatorOnly` in the endpoint table (the table guard runs
//! the operator-read admission and the peer proof before this handler), and
//! relay `artifact_read` / `artifact_versions`, which prove the operator
//! again and re-prove the stored scope against the conversation in view —
//! the board is never less strict than the RPC. The conversation is a query
//! field, never a path or header the stored row could be trusted from.
//!
//! Every response is `nosniff` and uncached; HTML and SVG also carry a
//! restrictive `Content-Security-Policy: sandbox` (no script, no network)
//! so a direct navigation to the URL cannot run agent-written code.

use base64::Engine;
use serde_json::json;
use tiny_http::{Header, Request};

use super::{err_response, home, json_response, HttpResp};
use crate::client;

fn query_conversation(request: &Request) -> Option<String> {
    let query = request.url().split_once('?')?.1;
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix("conversation="))
        .filter(|v| !v.is_empty() && v.len() <= 64)
        .map(str::to_string)
}

pub(super) fn read(
    request: &Request,
    state_dir: &std::path::Path,
    id: &str,
    version: &str,
) -> HttpResp {
    let Some(conversation) = query_conversation(request) else {
        return err_response(400, "conversation is required");
    };
    let Ok(version) = version.parse::<i64>() else {
        return err_response(400, "bad artifact version");
    };
    let reply = match client::rpc(
        state_dir,
        "artifact_read",
        json!({"id": id, "version": version, "conversation": conversation}),
    ) {
        Ok(v) => v,
        Err(e) => return home::rpc_err(&e, "artifact_read"),
    };
    let (Some(content), Some(mime)) = (reply["content"].as_str(), reply["mime"].as_str()) else {
        return err_response(500, "daemon artifact reply is malformed");
    };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(content) else {
        return err_response(500, "daemon artifact reply is malformed");
    };
    let mut resp = HttpResp::from_data(bytes);
    let header = |name: &str, value: &str| Header::from_bytes(name, value).unwrap();
    resp.add_header(header("Content-Type", mime));
    resp.add_header(header("X-Content-Type-Options", "nosniff"));
    resp.add_header(header("Cache-Control", "private, no-store"));
    if mime.starts_with("text/html") || mime.starts_with("image/svg") {
        resp.add_header(header(
            "Content-Security-Policy",
            "sandbox; default-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:",
        ));
    }
    resp
}

pub(super) fn versions(request: &Request, state_dir: &std::path::Path, id: &str) -> HttpResp {
    let Some(conversation) = query_conversation(request) else {
        return err_response(400, "conversation is required");
    };
    match client::rpc(
        state_dir,
        "artifact_versions",
        json!({"id": id, "conversation": conversation}),
    ) {
        Ok(v) => with_nosniff(json_response(v)),
        Err(e) => home::rpc_err(&e, "artifact_versions"),
    }
}

fn with_nosniff(mut resp: HttpResp) -> HttpResp {
    resp.add_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
    resp
}
