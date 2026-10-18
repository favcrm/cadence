//! Public app directory for a hosted board.
//!
//! The directory is a page in this board's own UI: `/apps/explore` and
//! `/apps/catalog/<id>` load for anyone, and that UI reads the catalogue
//! through `/api/public-apps` on this same origin. The browser talks only to
//! the board; the board is the one that reaches the catalogue, with no cookies
//! and no caller-chosen URL. Signing in is needed to install, not to browse.
//!
//! The catalogue is the board's own API, or a catalogue the operator names. It
//! is never the signed-in web app: this directory is public, so the app origin
//! the worker hands the board for its own callbacks is deliberately not read
//! here.

use tiny_http::{Header, Response, StatusCode};

use super::write_path::HttpResp;

fn clean_origin(raw: Option<&str>) -> Option<String> {
    let raw = raw.filter(|v| !v.trim().is_empty())?;
    let raw = raw.trim().trim_end_matches('/');
    if raw.contains(['?', '#', '@', ' ', '\\']) || raw.matches("//").count() != 1 {
        return None;
    }
    Some(raw.to_string())
}

fn reachable_catalog(raw: &str) -> bool {
    raw.starts_with("https://")
        || raw.starts_with("http://127.0.0.1")
        || raw.starts_with("http://localhost")
}

/// A hosted board has no internet: when the issuer is exactly
/// `http://api.internal`, that outbound handler is the catalogue. Otherwise the
/// catalogue is whatever `AGENTICOS_BOARD_CATALOG_ORIGIN` names — an operator's
/// own API, or a loopback rig. `AGENTICOS_BOARD_API_ORIGIN` is the signed-in
/// web app the worker hands the board for its own callbacks; it is not a
/// catalogue, and reading it here is what once pointed the public directory at
/// the private app.
pub(super) fn catalog_origin_from(catalog: Option<&str>, issuer: Option<&str>) -> Option<String> {
    let issuer = clean_origin(issuer);
    if issuer.as_deref() == Some("http://api.internal") {
        return issuer;
    }
    clean_origin(catalog).filter(|raw| reachable_catalog(raw))
}

fn catalog_origin() -> Option<String> {
    catalog_origin_from(
        std::env::var("AGENTICOS_BOARD_CATALOG_ORIGIN")
            .ok()
            .as_deref(),
        std::env::var("AGENTICOS_BOARD_ISSUER").ok().as_deref(),
    )
}

/// The directory's own pages. Their shell is served before the session gate, so
/// a visitor with no account can load the UI that renders the catalogue.
pub(super) fn is_directory_page(path: &str) -> bool {
    path == "/apps/explore" || directory_id(path, "/apps/catalog/").is_some()
}

/// The catalogue read that page makes, on this same origin. `/` and `/apps` are
/// the board's own front door and are not here.
pub(super) fn is_data_route(path: &str) -> bool {
    path == "/api/public-apps" || directory_id(path, "/api/public-apps/").is_some()
}

fn directory_id<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    let id = path.strip_prefix(prefix)?;
    catalog_id(id).then_some(id)
}

fn catalog_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('.')
        && !id.contains("..")
        && !id.contains('/')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

const BODY_CAP: u64 = 1024 * 1024;

/// One catalogue read, already answered. The catalogue answering 404 means that
/// app does not exist, which is a different answer from a catalogue that is
/// unreachable or unwell.
enum Upstream {
    Ok(Vec<u8>),
    NotFound,
    Failed,
}

fn fetch_upstream(url: &str) -> Upstream {
    use std::time::Duration;
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(8)))
        .max_redirects(0)
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let Ok(mut response) = agent.get(url).call() else {
        return Upstream::Failed;
    };
    match response.status().as_u16() {
        200 => {}
        404 => return Upstream::NotFound,
        _ => return Upstream::Failed,
    }
    match response
        .body_mut()
        .with_config()
        .limit(BODY_CAP)
        .read_to_vec()
    {
        Ok(body) => Upstream::Ok(body),
        Err(_) => Upstream::Failed,
    }
}

fn json(status: u16, body: Vec<u8>) -> HttpResp {
    let mut resp = Response::from_data(body).with_status_code(StatusCode(status));
    resp.add_header(Header::from_bytes("Content-Type", "application/json; charset=utf-8").unwrap());
    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    resp
}

/// A failure the UI can name. It owns the copy for each code, so nothing here
/// invents an app list or a success.
fn json_error(status: u16, code: &str) -> HttpResp {
    json(
        status,
        format!(r#"{{"ok":false,"error":"{code}"}}"#).into_bytes(),
    )
}

/// The status mapping, decided from the catalogue's answer alone. Pure, so it
/// is tested without a socket.
fn data_reply(upstream: Upstream) -> HttpResp {
    match upstream {
        Upstream::Ok(body) => json(200, body),
        Upstream::NotFound => json_error(404, "no_such_app"),
        Upstream::Failed => json_error(502, "catalogue_unavailable"),
    }
}

/// `Some` for a catalogue read: the catalogue's own JSON, or the one code that
/// says why the board could not get it. `None` for every other path, which
/// keeps its session gate.
pub(super) fn data_response(path: &str) -> Option<HttpResp> {
    if !is_data_route(path) {
        return None;
    }
    let Some(origin) = catalog_origin() else {
        return Some(json_error(503, "no_catalog"));
    };
    let suffix = path.strip_prefix("/api/public-apps")?;
    Some(data_reply(fetch_upstream(&format!("{origin}/public/apps{suffix}"))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_directory_pages_and_their_read_are_public() {
        assert!(is_directory_page("/apps/explore"));
        assert!(is_directory_page("/apps/catalog/social-content"));
        // The board's own front door is not the catalogue.
        assert!(!is_directory_page("/"));
        assert!(!is_directory_page("/apps"));
        assert!(!is_directory_page("/apps/manage/inst"));
        assert!(!is_directory_page("/apps/catalog/../admin"));
        assert!(!is_directory_page("/apps/catalog/a/b"));
        // Only this read answers without a session.
        assert!(is_data_route("/api/public-apps"));
        assert!(is_data_route("/api/public-apps/social-content"));
        assert!(!is_data_route("/api/app-home"));
        assert!(!is_data_route("/api/public-apps/../admin"));
        assert!(!is_data_route("/api/public-apps/a/b"));
        assert!(!is_data_route("/api/public-apps/.hidden"));
        // Pages and reads are different namespaces, so neither can stand in
        // for the other.
        assert!(!is_data_route("/apps/explore"));
        assert!(!is_directory_page("/api/public-apps"));
    }

    #[test]
    fn the_catalog_is_the_board_api_or_a_named_catalog_never_the_web_app() {
        // A hosted board reads its own API, whatever else is set.
        assert_eq!(
            catalog_origin_from(None, Some("http://api.internal")).as_deref(),
            Some("http://api.internal")
        );
        assert_eq!(
            catalog_origin_from(
                Some("https://api-v2.agenticos.hk"),
                Some("http://api.internal"),
            )
            .as_deref(),
            Some("http://api.internal")
        );
        // A rig or a self-hosted board names its own catalogue.
        assert_eq!(
            catalog_origin_from(Some("https://api-v2.agenticos.hk/"), None).as_deref(),
            Some("https://api-v2.agenticos.hk")
        );
        assert_eq!(
            catalog_origin_from(Some("http://127.0.0.1:8812"), None).as_deref(),
            Some("http://127.0.0.1:8812")
        );
        // The signed-in web app is not a catalogue, and an issuer alone is not
        // one either.
        assert!(catalog_origin_from(None, Some("https://app-v2.agenticos.hk")).is_none());
        assert!(catalog_origin_from(None, Some("https://app-v2.agenticos.hk/")).is_none());
        // Still refused: plain http off loopback, query strings, lookalikes.
        assert!(catalog_origin_from(Some("http://evil.example"), None).is_none());
        assert!(catalog_origin_from(Some("https://api.example/?x=1"), None).is_none());
        assert!(catalog_origin_from(None, Some("http://api.internal.evil")).is_none());
        assert!(catalog_origin_from(None, Some("http://api.internal/public/apps")).is_none());
    }

    /// One canned HTTP response on loopback, so the catalogue read is exercised
    /// against a real socket instead of a mock.
    fn catalog_stub(status_line: &str, body: &str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (status_line, body) = (status_line.to_string(), body.to_string());
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        format!("http://{addr}/public/apps")
    }

    #[test]
    fn the_catalog_status_decides_between_missing_and_broken() {
        assert!(matches!(
            fetch_upstream(&catalog_stub("404 Not Found", r#"{"ok":false}"#)),
            Upstream::NotFound
        ));
        assert!(matches!(
            fetch_upstream(&catalog_stub("500 Internal Server Error", "{}")),
            Upstream::Failed
        ));
        match fetch_upstream(&catalog_stub("200 OK", r#"{"ok":true}"#)) {
            Upstream::Ok(body) => assert_eq!(body, br#"{"ok":true}"#),
            _ => panic!("a 200 with a body is the one answer that carries it"),
        }
    }

    #[test]
    fn a_missing_app_is_a_404_while_an_unreachable_catalog_is_not() {
        assert_eq!(data_reply(Upstream::NotFound).status_code().0, 404);
        assert_eq!(data_reply(Upstream::Failed).status_code().0, 502);
        assert_eq!(
            data_reply(Upstream::Ok(br#"{"ok":true}"#.to_vec()))
                .status_code()
                .0,
            200
        );
    }
}
