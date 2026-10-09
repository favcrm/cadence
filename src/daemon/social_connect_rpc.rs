//! CAD-1290: connect Instagram through AgenticOS and choose the publishing
//! account, from the board, with no CLI.
//!
//! Three operator-only methods (the company comes from the resolver's own
//! credential or lease, so a destination of another company never lists). None adds authority: the connect link is a
//! URL the owner opens on AgenticOS (which enforces owner-only itself), the
//! destination list is the company-scoped read the hosted lease door already
//! serves, and "use for publishing" writes the same publication binding
//! receipt `app binding create` and `app binding publish` write, through the
//! same store calls, so a rebind still drops grants as it does today.
//!
//! The host composes every URL. `return_to` must be on this board's own
//! public host; the AgenticOS origin and company come from the board
//! identity, never from the caller.

use super::app_bindings_rpc::strict_fields;
use super::social_publish_start::PublishTarget;
use super::*;
use crate::issue::{app, app_catalog::workspace};
use crate::platform::agenticos_external::PLATFORM;

const TOOLKIT: &str = "instagram";
const TIMEZONE: &str = "Asia/Hong_Kong";

/// Percent-encode everything but RFC 3986 unreserved bytes.
fn pct(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() * 3);
    for b in raw.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// An https origin with no path, query, userinfo or backslash.
fn valid_origin(origin: &str) -> bool {
    let origin = origin.trim_end_matches('/');
    origin
        .strip_prefix("https://")
        .is_some_and(|host| !host.is_empty() && !host.contains(['/', '?', '#', '@', '\\']))
}

/// The AgenticOS owner connect link for this board. `return_to` is accepted
/// only as `https://<this board's host>/…`: plain printable ASCII, no
/// userinfo, no fragment, no backslash.
pub(super) fn compose_connect_url(
    issuer: &str,
    company: &str,
    board_host: &str,
    return_to: &str,
) -> Result<String> {
    let refuse = || Error::rejected("return_to must be a page on this board");
    let rest = return_to.strip_prefix("https://").ok_or_else(refuse)?;
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    if authority != board_host
        || return_to.len() > 1024
        || !return_to.bytes().all(|b| (0x21..0x7f).contains(&b))
        || return_to.contains(['#', '\\'])
        || path.starts_with('/')
    {
        return Err(refuse());
    }
    let origin = issuer.trim_end_matches('/');
    if !valid_origin(origin)
        || company.is_empty()
        || !company
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(Error::rejected("the AgenticOS connect link is unavailable"));
    }
    Ok(format!(
        "{origin}/v2/account/{company}/connections/connect?toolkit={TOOLKIT}&allow_publish=1&return_to={}",
        pct(return_to)
    ))
}

impl Shared {
    /// Hosted means the credentialless hosted lease transport is attached
    /// (the same admission the hosted media door uses) and publish can read
    /// destinations. Local boards keep the local connections page.
    fn hosted_publish(&self) -> bool {
        self.social_media_resolver.is_some()
            && self
                .platforms
                .get(PLATFORM)
                .is_some_and(|adapter| adapter.app_credentialless_account("hosted"))
    }

    pub(super) fn rpc_social_connect(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        pid: u32,
    ) -> Result<Value> {
        self.operator_connection("social publishing setup", params, pid)?;
        match method {
            "social_connect_link" => {
                strict_fields(params, &["return_to"])?;
                if !self.hosted_publish() {
                    return Ok(json!({"hosted": false}));
                }
                let board = crate::board_identity::read_config(&self.state_dir)?;
                // The hosted issuer is the container-internal `http://api.internal`,
                // never a browser address. The platform injects the public API
                // origin as AGENTICOS_BOARD_API_ORIGIN; a self-hosted board with
                // real egress has an https issuer, which is the same origin.
                let origin = std::env::var("AGENTICOS_BOARD_API_ORIGIN")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| board.issuer.clone());
                if !valid_origin(&origin) {
                    return Ok(json!({"hosted": true, "unavailable": true}));
                }
                let url = compose_connect_url(
                    &origin,
                    &board.company,
                    &board.host,
                    required_str(params, "return_to")?,
                )?;
                Ok(json!({"hosted": true, "url": url}))
            }
            "social_destinations" => {
                strict_fields(params, &[])?;
                let listed = self
                    .social_media_resolver
                    .as_ref()
                    .and_then(|resolver| resolver.list_active(TOOLKIT));
                Ok(match listed {
                    None => json!({"unavailable": true, "destinations": []}),
                    Some(rows) => json!({"unavailable": false,
                        "destinations": rows.iter().map(|row| json!({
                            "destination_id": row.destination_id,
                            "label": row.display_name,
                            "toolkit": row.toolkit,
                        })).collect::<Vec<_>>()}),
                })
            }
            "app_binding_use_destination" => self.use_destination(params),
            _ => Err(Error::rejected("unknown social publishing method")),
        }
    }

    /// Create or replace the install's publication binding for the active
    /// context so it publishes to one of this company's destinations. The
    /// destination must be in the company-scoped list the door returns now;
    /// the label comes from that list, never from the request.
    fn use_destination(self: &Arc<Self>, params: &Value) -> Result<Value> {
        strict_fields(
            params,
            &[
                "install_id",
                "context_id",
                "destination_id",
                "request_id",
                "expected_revision",
            ],
        )?;
        let install = required_str(params, "install_id")?;
        crate::proto::identifier(install, "installation id")?;
        let context = match params.get("context_id") {
            None => None,
            Some(Value::String(id)) => Some(id.as_str()),
            Some(_) => {
                return Err(Error::rejected(
                    "context_id must be a non-null string when present",
                ))
            }
        };
        let destination = required_str(params, "destination_id")?;
        let resolver = self
            .social_media_resolver
            .clone()
            .ok_or_else(|| Error::rejected("capability_unavailable: no publish resolver"))?;
        let row = resolver
            .list_active(TOOLKIT)
            .ok_or_else(|| Error::rejected("capability_unavailable: destinations unavailable"))?
            .into_iter()
            .find(|row| row.destination_id == destination)
            .ok_or_else(|| {
                Error::rejected(
                    "grant_binding_mismatch: that is not one of this company's connected Instagram accounts",
                )
            })?;
        // Exactly one publishable row must carry it (the send path's rule).
        resolver
            .resolve(TOOLKIT, destination)
            .into_result()
            .map_err(|e| Error::rejected(e.to_string()))?;
        let label = row.display_name.trim();
        let publish = json!({
            "destination_id": destination,
            "destination_label": if label.is_empty() { destination } else { label },
            "toolkit": TOOLKIT,
            "timezone": TIMEZONE,
        });
        PublishTarget::parse_hosted(&publish)?;
        let expected = params.get("expected_revision");
        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_runtime_snapshot(&pm, install, |bundle, files| {
            let _custody = self
                .platform_custody_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let manifest = app::parse_manifest(
                files
                    .get("app.md")
                    .ok_or_else(|| Error::rejected("installation manifest unavailable"))?,
            )?;
            let mut sends = manifest
                .capabilities
                .iter()
                .filter(|(_, need)| need.effect == "send");
            let (slot, _) = sends
                .next()
                .ok_or_else(|| Error::rejected("installation declares no send capability"))?;
            if sends.next().is_some() {
                return Err(Error::rejected(
                    "social publish requires exactly one declared send slot",
                ));
            }
            let current = self.store.app_binding_for_slot(
                install,
                context,
                slot,
                required_str(bundle, "digest")?,
            )?;
            let mut config;
            match (current, expected) {
                (Some(proof), Some(rev)) if rev.as_i64() == Some(proof.revision) => {
                    config = proof.config.clone();
                    config["publish"] = publish;
                    self.store
                        .app_binding_update(install, &proof.id, proof.revision, &config)
                }
                (Some(_), _) | (None, Some(_)) => Err(Error::rejected(
                    "stale: the publication binding changed — reload and try again",
                )),
                (None, None) => {
                    let request = required_str(params, "request_id")?;
                    crate::proto::identifier(request, "request id")?;
                    let local = self
                        .connection_list_locked()?
                        .into_iter()
                        .find(|row| row["provider"] == "local")
                        .and_then(|row| row["id"].as_str().map(str::to_owned))
                        .ok_or_else(|| Error::rejected("publication connection unavailable"))?;
                    config =
                        self.app_binding_config(install, context, slot, &local, bundle, files)?;
                    config["publish"] = publish;
                    self.store
                        .app_binding_create(install, context, slot, &config, request)
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::compose_connect_url;

    const ISSUER: &str = "https://api-v2.agenticos.hk";
    const HOST: &str = "essential-foods.cadencecloud.app";

    fn link(return_to: &str) -> crate::error::Result<String> {
        compose_connect_url(ISSUER, "ws_abc-123", HOST, return_to)
    }

    #[test]
    fn the_link_is_composed_from_the_board_identity_and_encodes_return_to() {
        let url =
            link("https://essential-foods.cadencecloud.app/app-installations/i1?view=settings")
                .unwrap();
        assert_eq!(
            url,
            "https://api-v2.agenticos.hk/v2/account/ws_abc-123/connections/connect?toolkit=instagram&allow_publish=1&return_to=https%3A%2F%2Fessential-foods.cadencecloud.app%2Fapp-installations%2Fi1%3Fview%3Dsettings"
        );
    }

    #[test]
    fn return_to_off_this_board_is_refused() {
        for bad in [
            "https://evil.example/x",
            "https://essential-foods.cadencecloud.app.evil.example/x",
            "https://essential-foods.cadencecloud.app@evil.example/x",
            "https://evil.example/x#https://essential-foods.cadencecloud.app/",
            "http://essential-foods.cadencecloud.app/x",
            "//essential-foods.cadencecloud.app/x",
            "https://essential-foods.cadencecloud.app//evil.example",
            "https://essential-foods.cadencecloud.app/x\\y",
            "https://essential-foods.cadencecloud.app/x y",
            "/app-installations/i1",
        ] {
            assert!(link(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn a_non_https_platform_origin_or_odd_company_is_refused() {
        let ok = "https://essential-foods.cadencecloud.app/x";
        assert!(compose_connect_url("http://api.agenticos.hk", "ws1", HOST, ok).is_err());
        // The hosted issuer is container-internal, never a browser address.
        assert!(compose_connect_url("http://api.internal", "ws1", HOST, ok).is_err());
        assert!(compose_connect_url("https://api.example/extra", "ws1", HOST, ok).is_err());
        assert!(compose_connect_url(ISSUER, "ws1/../x", HOST, ok).is_err());
        assert!(compose_connect_url(ISSUER, "", HOST, ok).is_err());
    }
}
