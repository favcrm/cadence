//! Catalogue origin resolution and versions metadata.
//!
//! The application concern owns the configured origin and the versions read.
//! The UI keeps only the public-catalog HTTP proxy and its route validation,
//! so catalogue authorization no longer calls into the board presentation.

/// Hosted boards retain their internal metadata route. Other boards use the
/// operator's explicit catalog, or the public AgenticOS catalog by default.
/// Neither a signed-in web-app origin nor an arbitrary issuer is a catalog.
fn catalog_origin_from(catalog: Option<&str>, issuer: Option<&str>) -> Option<String> {
    if let Some(issuer) = issuer.map(str::trim).filter(|v| !v.is_empty()) {
        if issuer.trim_end_matches('/') == "http://api.internal" {
            return Some("http://api.internal".to_string());
        }
    }
    let raw = catalog
        .filter(|v| !v.trim().is_empty())
        .unwrap_or("https://api-v2.agenticos.hk");
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

pub(crate) fn catalog_origin() -> Option<String> {
    catalog_origin_from(
        std::env::var("AGENTICOS_BOARD_CATALOG_ORIGIN")
            .ok()
            .as_deref(),
        std::env::var("AGENTICOS_BOARD_ISSUER").ok().as_deref(),
    )
}

pub(crate) fn catalog_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('.')
        && !id.contains("..")
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Versions metadata for one catalogue id. Transport failure, a non-200
/// status and a non-JSON body are the same refusal: metadata was not read,
/// so it cannot authorize a catalogue claim. This is not a general client.
pub(crate) fn fetch_versions_at(origin: &str, id: &str) -> crate::error::Result<serde_json::Value> {
    use crate::error::Error;
    if !catalog_id(id) || !catalogue_id_for_versions(id) {
        return Err(Error::rejected("catalogue metadata is unavailable"));
    }
    let origin = origin.trim().trim_end_matches('/');
    if origin.is_empty() || origin.contains(['?', '#', ' ', '@']) {
        return Err(Error::rejected("catalogue metadata is unavailable"));
    }
    let url = format!("{origin}/public/apps/{id}/versions");
    fetch_json(&url).ok_or_else(|| Error::rejected("catalogue metadata is unavailable"))
}

fn catalogue_id_for_versions(id: &str) -> bool {
    super::catalogue::catalogue_id_ok(id)
}

const VERSIONS_BODY_CAP: u64 = 1024 * 1024;

fn fetch_json(url: &str) -> Option<serde_json::Value> {
    use std::time::Duration;
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(8)))
        .max_redirects(0)
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut response = agent.get(url).call().ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(VERSIONS_BODY_CAP)
        .read_to_vec()
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

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
            catalog_origin_from(Some("https://catalog.example/"), None).as_deref(),
            Some("https://catalog.example")
        );
        assert_eq!(
            catalog_origin_from(
                Some("http://127.0.0.1:8787"),
                Some("https://ignored.example")
            )
            .as_deref(),
            Some("http://127.0.0.1:8787")
        );
        // Empty environment overrides behave like an unset override.
        for catalog in [Some(""), Some("   ")] {
            assert_eq!(
                catalog_origin_from(catalog, None).as_deref(),
                Some("https://api-v2.agenticos.hk")
            );
        }
    }

    #[test]
    fn invalid_explicit_catalog_refuses_fallback_except_for_hosted_boards() {
        for invalid in ["http://catalog.example", "https://catalog.example/?x=1"] {
            for issuer in [
                None,
                Some("https://app-v2.agenticos.hk"),
                Some("http://api.internal.evil"),
            ] {
                assert_eq!(
                    catalog_origin_from(Some(invalid), issuer),
                    None,
                    "invalid explicit catalog must refuse: {invalid}, issuer: {issuer:?}"
                );
                assert_eq!(
                    catalog_origin_from(None, issuer).as_deref(),
                    Some("https://api-v2.agenticos.hk")
                );
            }

            assert_eq!(
                catalog_origin_from(Some(invalid), Some("http://api.internal")).as_deref(),
                Some("http://api.internal"),
                "hosted metadata routing must precede explicit catalog validation"
            );
        }
    }
}
