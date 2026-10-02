//! CAD-1006: the board's private screen mount — the operator mint POST
//! and the nonce-consumed frame GET that serves the CSP-pinned document.
//!
//! Two routes, two different authorities — and the session is proven by
//! relaying the request's real credential to the daemon's native check,
//! never by trusting a caller-supplied id:
//!
//! - `POST /api/app-installations/<id>/screens/<tag>/mount` —
//!   `RouteClass::OperatorOnly`, body `{}`. The board takes the request's
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
fn request_credentials(request: &Request, opts: &ServeOpts) -> Option<(String, String, &'static str)> {
    let origin = operator::request_origin_kind(request, opts)?;
    let token = operator::session_token_for(request, opts, origin)?;
    let key = super::header_value(request, operator::SESSION_HEADER).unwrap_or_default();
    Some((token, key, origin.as_str()))
}

/// The mint handler — body `{}` only; scope (`install_id`,`tag`) from the
/// URL; session credentials relayed from the proven request. `admit`'s
/// OperatorOnly gate has already run before this handler is reached.
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
    let ok = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .is_some_and(|v| v.as_object().is_some_and(|m| m.is_empty()));
    if !ok {
        return err_response(400, "screen mount takes no fields — send {}");
    }
    let Some((token, key, origin)) = request_credentials(request, opts) else {
        return err_response(403, "screen mount needs a live operator session");
    };
    let generation = next_generation();
    match client::rpc(
        state_dir,
        "app_screen_mint",
        json!({
            "install_id": install_id,
            "tag": tag,
            "token": token,
            "key": key,
            "origin": origin,
            "generation": generation,
        }),
    ) {
        Ok(v) => super::json_response(v),
        Err(e) => home::rpc_err(&e, "app_screen_mint"),
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
    let out = match client::rpc(
        state_dir,
        "app_screen_consume",
        json!({"nonce": nonce}),
    ) {
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
    let assets = out["assets"].as_object();
    let css = assets
        .and_then(|a| a.iter().find(|(k, _)| k.ends_with(".css")))
        .and_then(|(_, v)| v.as_str())
        .unwrap_or("");
    let js = assets
        .and_then(|a| a.get("client.js"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let origin_str = board_origin(request, opts);
    let generation = out["generation"].as_u64().unwrap_or(0);
    let tag = out["tag"].as_str().unwrap_or("");
    let html = match crate::daemon::render_frame_html(
        &csp_nonce,
        tag,
        &bridge_nonce,
        generation,
        css,
        js,
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
    let csp = format!(
        "default-src 'none'; script-src 'nonce-{csp}'; style-src 'unsafe-inline'; \
         img-src data:; font-src data:; connect-src 'none'; media-src 'none'; \
         object-src 'none'; frame-src 'none'; worker-src 'none'; base-uri 'none'; \
         form-action 'none'; frame-ancestors {origin}",
        csp = csp_nonce,
        origin = origin_str,
    );
    resp.add_header(Header::from_bytes("Content-Security-Policy", csp).unwrap());
    resp
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
fn origin_for(class: Option<Origin>, opts: &ServeOpts) -> String {
    match class {
        Some(Origin::Public) => match &opts.public {
            Some(public) => {
                format!("{}://{}", operator::public_scheme(&public.host), public.host)
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
}

/// A fresh 128-bit CSP nonce, base64url no-pad — one per document.
fn fresh_csp_nonce() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("getrandom");
    b64url(&bytes)
}

fn b64url(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((bytes.len() * 4 + 2) / 3);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
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
