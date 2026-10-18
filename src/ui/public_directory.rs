//! CAD-1471: the public app directory is a read-only document surface.
//! It does not admit workspace reads or writes. The browser only talks to
//! this board. The board reads the configured catalog origin itself, with
//! no cookies and no caller-chosen URL.

/// SPA documents and the static files they need, plus nothing else.
pub(super) fn directory_surface(path: &str) -> bool {
    if path == "/apps/explore"
        || path == "/favicon.svg"
        || path == "/favicon.ico"
        || path.starts_with("/assets/")
    {
        return path != "/assets/" && !path.contains("..");
    }
    let Some(id) = path.strip_prefix("/apps/catalog/") else {
        return false;
    };
    !id.is_empty()
        && !id.contains('/')
        && !id.contains("..")
        && !id.starts_with('.')
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// A hosted board has no internet: when the issuer is exactly
/// `http://api.internal`, that outbound handler is the catalog. Otherwise the
/// catalog is whatever `AGENTICOS_BOARD_CATALOG_ORIGIN` names — an operator's
/// own API, or a loopback rig. `AGENTICOS_BOARD_API_ORIGIN` is the signed-in
/// web app this board talks back to; it is not a catalog, and reading it here
/// pointed the public directory at the private app.
pub(super) fn catalog_origin_from(catalog: Option<&str>, issuer: Option<&str>) -> Option<String> {
    if let Some(issuer) = issuer.map(str::trim).filter(|v| !v.is_empty()) {
        if issuer.trim_end_matches('/') == "http://api.internal" {
            return Some("http://api.internal".to_string());
        }
    }
    let raw = catalog.filter(|v| !v.trim().is_empty())?;
    let raw = raw.trim().trim_end_matches('/');
    if raw.contains(['?', '#', '@', ' ']) {
        return None;
    }
    let loopback = raw.starts_with("http://127.0.0.1") || raw.starts_with("http://localhost");
    if !raw.starts_with("https://") && !loopback {
        return None;
    }
    Some(raw.to_string())
}

pub(super) fn catalog_origin() -> Option<String> {
    catalog_origin_from(
        std::env::var("AGENTICOS_BOARD_CATALOG_ORIGIN")
            .ok()
            .as_deref(),
        std::env::var("AGENTICOS_BOARD_ISSUER").ok().as_deref(),
    )
}

const CATALOG_BODY_CAP: u64 = 8 * 1024 * 1024;

/// `/api/public-apps` mirrors the catalog's public reads. Anything else is
/// refused here so a bad id cannot fall through into a private board route.
pub(super) fn is_public_catalog_route(path: &str) -> bool {
    path == "/api/public-apps" || path.starts_with("/api/public-apps/")
}

pub(super) fn proxy_public_catalog(path: &str, raw_query: &str) -> super::write_path::HttpResp {
    let Some(upstream) = catalog_upstream(path, raw_query) else {
        return super::serve::err_response(404, "No such app.");
    };
    let Some(origin) = catalog_origin() else {
        return super::serve::err_response(503, "public directory is not configured");
    };
    fetch_public_catalog(&format!("{origin}{upstream}"))
}

fn catalog_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('.')
        && !id.contains("..")
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn digest_param(raw_query: &str) -> Option<String> {
    if raw_query.is_empty() || raw_query.contains('&') {
        return None;
    }
    let (key, value) = raw_query.split_once('=')?;
    if key != "digest" {
        return None;
    }
    let value = super::serve::pct_decode(value)?;
    let hex = value.strip_prefix("sha256:")?;
    if hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        Some(value)
    } else {
        None
    }
}

/// The upstream path, or `None` when the request is not one of the three
/// public catalog reads.
fn catalog_upstream(path: &str, raw_query: &str) -> Option<String> {
    let rest = path.strip_prefix("/api/public-apps")?;
    if rest.is_empty() {
        return raw_query.is_empty().then(|| "/public/apps".to_string());
    }
    let rest = rest.strip_prefix('/')?;
    if let Some(id) = rest.strip_suffix("/release") {
        if !catalog_id(id) {
            return None;
        }
        let digest = digest_param(raw_query)?;
        return Some(format!("/public/apps/{id}/release?digest={digest}"));
    }
    if !catalog_id(rest) || rest.contains('/') || !raw_query.is_empty() {
        return None;
    }
    Some(format!("/public/apps/{rest}"))
}

fn fetch_public_catalog(url: &str) -> super::write_path::HttpResp {
    use std::time::Duration;
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(8)))
        .max_redirects(0)
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut response = match agent.get(url).call() {
        Ok(response) => response,
        Err(_) => return super::serve::err_response(502, "The public directory didn't load."),
    };
    let status = response.status().as_u16();
    if !matches!(status, 200 | 404 | 409) {
        return super::serve::err_response(502, "The public directory didn't load.");
    }
    let bytes = match response
        .body_mut()
        .with_config()
        .limit(CATALOG_BODY_CAP)
        .read_to_vec()
    {
        Ok(bytes) => bytes,
        Err(_) => return super::serve::err_response(502, "The public directory didn't load."),
    };
    if serde_json::from_slice::<serde_json::Value>(&bytes).is_err() {
        return super::serve::err_response(502, "The public directory didn't load.");
    }
    let mut resp =
        tiny_http::Response::from_data(bytes).with_status_code(tiny_http::StatusCode(status));
    resp.add_header(tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap());
    resp.add_header(tiny_http::Header::from_bytes("Cache-Control", "no-store").unwrap());
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_is_only_the_directory_and_its_assets() {
        assert!(directory_surface("/apps/explore"));
        assert!(directory_surface("/apps/catalog/social-content"));
        assert!(directory_surface("/assets/index.js"));
        assert!(!directory_surface("/"));
        assert!(!directory_surface("/apps"));
        assert!(!directory_surface("/apps/manage/inst"));
        assert!(!directory_surface("/api/apps"));
        assert!(!directory_surface("/api/app-installations/upload"));
        assert!(!directory_surface("/apps/catalog/a/b"));
        assert!(!directory_surface("/apps/catalog/.."));
        assert!(!directory_surface("/assets/"));
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
                Some("http://api.internal")
            )
            .as_deref(),
            Some("http://api.internal")
        );
        // A rig or a self-hosted board names its own catalog.
        assert_eq!(
            catalog_origin_from(Some("https://api-v2.agenticos.hk/"), None).as_deref(),
            Some("https://api-v2.agenticos.hk")
        );
        assert_eq!(
            catalog_origin_from(
                Some("http://127.0.0.1:8787"),
                Some("https://ignored.example")
            )
            .as_deref(),
            Some("http://127.0.0.1:8787")
        );
        // The signed-in web app is not a catalog, and an issuer alone is not one.
        assert!(catalog_origin_from(None, Some("https://app-v2.agenticos.hk")).is_none());
        assert!(catalog_origin_from(None, Some("https://ignored.example")).is_none());
        // Still refused: plain http off loopback and query strings.
        assert!(catalog_origin_from(Some("http://evil.example"), None).is_none());
        assert!(catalog_origin_from(Some("https://api.example/?x=1"), None).is_none());
        assert!(catalog_origin_from(None, Some("http://api.internal.evil")).is_none());
    }

    #[test]
    fn catalog_proxy_only_forwards_the_three_public_reads() {
        assert_eq!(
            catalog_upstream("/api/public-apps", "").as_deref(),
            Some("/public/apps")
        );
        assert_eq!(
            catalog_upstream("/api/public-apps/social-content", "").as_deref(),
            Some("/public/apps/social-content")
        );
        assert_eq!(
            catalog_upstream(
                "/api/public-apps/social-content/release",
                "digest=sha256%3Aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            )
            .as_deref(),
            Some(
                "/public/apps/social-content/release?digest=sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            )
        );
        assert!(catalog_upstream("/api/public-apps", "x=1").is_none());
        assert!(catalog_upstream("/api/public-apps/../admin", "").is_none());
        assert!(catalog_upstream("/api/public-apps/a/b", "").is_none());
        assert!(
            catalog_upstream("/api/public-apps/social-content/release", "digest=nope").is_none()
        );
        assert!(catalog_upstream("/api/other", "").is_none());
    }
}
