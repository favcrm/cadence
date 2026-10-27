//! CAD-1423: Web Push subscription checks. The platform (`http://api.internal`)
//! holds the VAPID private key and performs the encryption and send; this
//! module never loads, signs with, or sees one. It validates the browser's
//! subscription before the native side stores it.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

use crate::error::{Error, Result};

/// Validate a browser's `p256dh` (65-byte uncompressed P-256 point) and `auth`
/// (16-byte secret), both base64url. Malformed keys are refused before any write.
pub fn subscription_keys(p256dh: &str, auth: &str) -> Result<()> {
    let invalid = || Error::invalid("invalid_request", "push subscription keys are not valid");
    if !valid_p256_point(p256dh) {
        return Err(invalid());
    }
    let secret = URL_SAFE_NO_PAD.decode(auth).map_err(|_| invalid())?;
    if secret.len() != 16 {
        return Err(invalid());
    }
    Ok(())
}

/// Whether `encoded` is an uncompressed P-256 point (65 bytes, base64url), the
/// form a Web Push application-server or subscription key takes.
pub fn valid_p256_point(encoded: &str) -> bool {
    let Ok(public) = URL_SAFE_NO_PAD.decode(encoded) else {
        return false;
    };
    public.len() == 65 && public[0] == 0x04 && p256::PublicKey::from_sec1_bytes(&public).is_ok()
}

/// Refuse any endpoint that is not an HTTPS URL on a known push-provider
/// host. Private, local and arbitrary destinations fail here, before any
/// network I/O. The allow-list is closed on purpose.
pub fn check_endpoint(endpoint: &str) -> Result<()> {
    let refused = || {
        Error::invalid(
            "invalid_request",
            "push endpoint is not an allowed provider",
        )
    };
    if endpoint.len() > 2048 || endpoint.chars().any(char::is_control) {
        return Err(refused());
    }
    let rest = endpoint.strip_prefix("https://").ok_or_else(refused)?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // Userinfo, an explicit port other than 443, and empty hosts are all refused.
    if authority.contains('@') || authority.contains(':') || authority.is_empty() {
        return Err(refused());
    }
    if !PROVIDER_HOSTS.contains(&authority) && !is_mozilla_push_shard(authority) {
        return Err(refused());
    }
    Ok(())
}

/// Mozilla's documented push shards: exactly `updates-<digits>.push.services.mozilla.com`.
fn is_mozilla_push_shard(host: &str) -> bool {
    let Some(label) = host.strip_suffix(".push.services.mozilla.com") else {
        return false;
    };
    let Some(digits) = label.strip_prefix("updates-") else {
        return false;
    };
    !digits.is_empty() && digits.len() <= 3 && digits.bytes().all(|b| b.is_ascii_digit())
}

/// Push provider hosts this build will deliver to. Closed list.
const PROVIDER_HOSTS: &[&str] = &[
    "fcm.googleapis.com",
    "push.services.mozilla.com",
    "updates.push.services.mozilla.com",
    "web.push.apple.com",
    "notify.windows.com",
];
