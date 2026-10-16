//! CAD-1471: the public app directory is a read-only document surface.
//! It does not admit workspace reads or writes. The catalog origin is the
//! platform API the browser fetches with credentials omitted.

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

/// `AGENTICOS_BOARD_API_ORIGIN` wins; otherwise an https board issuer.
/// Loopback http is admitted so a disposable rig can point at a local catalog.
pub(super) fn catalog_origin_from(api: Option<&str>, issuer: Option<&str>) -> Option<String> {
    let raw = api.filter(|v| !v.trim().is_empty()).or(issuer)?;
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
        std::env::var("AGENTICOS_BOARD_API_ORIGIN").ok().as_deref(),
        std::env::var("AGENTICOS_BOARD_ISSUER").ok().as_deref(),
    )
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
    fn origin_rejects_arbitrary_urls() {
        assert_eq!(
            catalog_origin_from(Some("https://api-v2.agenticos.hk/"), None).as_deref(),
            Some("https://api-v2.agenticos.hk")
        );
        assert_eq!(
            catalog_origin_from(Some("http://127.0.0.1:8787"), Some("https://ignored.example"))
                .as_deref(),
            Some("http://127.0.0.1:8787")
        );
        assert!(catalog_origin_from(Some("http://evil.example"), None).is_none());
        assert!(catalog_origin_from(Some("https://api.example/?x=1"), None).is_none());
        assert!(catalog_origin_from(None, Some("http://api.internal")).is_none());
    }

    #[test]
    fn board_csp_allows_only_configured_catalog_origin() {
        assert_eq!(super::super::serve::board_csp(None), super::super::serve::CSP);
        let with_origin = super::super::serve::board_csp(Some("http://127.0.0.1:8826"));
        assert!(with_origin.ends_with("; connect-src 'self' http://127.0.0.1:8826"));
    }
}
