//! CAD-1471/CAD-1638: the app catalogue is a signed-in read.
//! The container no longer serves an anonymous directory; every document and
//! `/api/public-apps*` request is admitted against the board session before
//! dispatch. The board reads the configured catalog origin itself, with
//! no cookies and no caller-chosen URL.

use crate::issue::app_catalog::client::{catalog_id, catalog_origin};

const CATALOG_BODY_CAP: u64 = 8 * 1024 * 1024;

/// `/api/public-apps` mirrors the catalog's public reads. Anything else is
/// refused here so a bad id cannot fall through into a private board route.
pub(super) fn is_public_catalog_route(path: &str) -> bool {
    path == "/api/public-apps" || path.starts_with("/api/public-apps/")
}

/// The fixed hosted package broker the platform answers inside the
/// container. It serves only the pinned release document — never metadata,
/// never another path — so `api.internal` stays a metadata door.
const HOSTED_PACKAGE_ORIGIN: &str = "http://catalogue-packages.internal";

pub(super) fn proxy_public_catalog(path: &str, raw_query: &str) -> super::write_path::HttpResp {
    let Some(upstream) = catalog_upstream(path, raw_query) else {
        return super::serve::err_response(404, "No such app.");
    };
    let Some(origin) = catalog_origin() else {
        return super::serve::err_response(503, "public directory is not configured");
    };
    // Only the exact, already-validated package read uses the closed broker.
    // Metadata stays on api.internal; self-hosted origins serve their own bytes.
    let fetch_origin = if origin == "http://api.internal" && upstream.contains("/release?") {
        HOSTED_PACKAGE_ORIGIN
    } else {
        &origin
    };
    fetch_public_catalog(&format!("{fetch_origin}{upstream}"))
}

/// The upstream path, or `None` when the request is not one of the closed
/// public catalog reads: the list, one app, its versions metadata, or one
/// exact release document. Versions carry no query and no bundle bytes.
/// The release document still requires both the exact version and digest.
fn catalog_upstream(path: &str, raw_query: &str) -> Option<String> {
    let rest = path.strip_prefix("/api/public-apps")?;
    if rest.is_empty() {
        return raw_query.is_empty().then(|| "/public/apps".to_string());
    }
    let rest = rest.strip_prefix('/')?;
    if let Some(id) = rest.strip_suffix("/versions") {
        if !catalog_id(id) || id.contains('/') || !raw_query.is_empty() {
            return None;
        }
        return Some(format!("/public/apps/{id}/versions"));
    }
    if let Some(id) = rest.strip_suffix("/release") {
        if !catalog_id(id) || id.contains('/') {
            return None;
        }
        let (version, digest) = release_query(raw_query)?;
        return Some(format!(
            "/public/apps/{id}/release?version={}&digest={}",
            query_component(&version),
            query_component(&digest)
        ));
    }
    if !catalog_id(rest) || rest.contains('/') || !raw_query.is_empty() {
        return None;
    }
    Some(format!("/public/apps/{rest}"))
}

fn version_token(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 40
        && version.bytes().all(
            |b| matches!(b, b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' | b'.' | b'_' | b'+' | b'-'),
        )
}

/// Exactly `version` and `digest`, once each, in either order. A digest-only
/// query is not a version pin and is refused.
fn release_query(raw_query: &str) -> Option<(String, String)> {
    if raw_query.is_empty() || raw_query.contains(['?', '#']) {
        return None;
    }
    let mut version = None;
    let mut digest = None;
    for pair in raw_query.split('&') {
        if pair.is_empty() {
            return None;
        }
        let (key, value) = pair.split_once('=')?;
        let decoded = super::serve::pct_decode(value)?;
        match key {
            "version" if version.is_none() => {
                if !version_token(&decoded) {
                    return None;
                }
                version = Some(decoded);
            }
            "digest" if digest.is_none() => {
                let hex = decoded.strip_prefix("sha256:")?;
                if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
                    return None;
                }
                digest = Some(decoded);
            }
            _ => return None,
        }
    }
    Some((version?, digest?))
}

fn query_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
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
    fn catalogue_proxy_refuses_unpinned_or_private_reads() {
        assert!(catalog_upstream("/api/public-apps", "x=1").is_none());
        assert!(catalog_upstream("/api/public-apps/../admin", "").is_none());
        assert!(catalog_upstream("/api/public-apps/a/b", "").is_none());
        assert!(catalog_upstream("/api/public-apps/social-content/versions", "x=1").is_none());
        assert!(catalog_upstream("/api/public-apps/social-content/versions/extra", "").is_none());
        assert!(
            catalog_upstream(
                "/api/public-apps/social-content/release",
                "digest=sha256%3Aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            )
            .is_none(),
            "a digest without a version is not a pinned release"
        );
        assert!(catalog_upstream(
            "/api/public-apps/social-content/release",
            "version=0.9.7&version=0.9.8&digest=sha256%3Aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        )
        .is_none());
        assert!(catalog_upstream(
            "/api/public-apps/social-content/release",
            "version=0.9.7&digest=sha256%3Aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa&extra=1"
        )
        .is_none());
        assert!(catalog_upstream("/api/public-apps/social-content/blob", "").is_none());
        assert!(catalog_upstream("/api/other", "").is_none());
    }
}
