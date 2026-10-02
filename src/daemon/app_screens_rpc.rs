//! CAD-1006 R3 (revised): the two operator-side screen verbs.
//!
//! `app_screen_mint` — `operator_connection`-gated. Relays the requesting
//! session's REAL bearer credential (`token` + `key` + `origin`) and runs
//! the daemon's native check itself — `Auth::check` for a keyed
//! loopback/tailnet session or `Auth::check_public` for the `__Host-`
//! public cookie — resolving the live `SessionView.id` (and, on the public
//! surface, requiring the asserted user's `BoardUser::is_operator()` role).
//! A caller-supplied session *id* is never accepted as authority. On
//! success it recomputes the live `bundle_digest`, requires
//! `app_capability_status(install_id, live_digest).state == "approved"`,
//! re-runs [`crate::issue::app_screen_pkg::extract`], and mints one
//! 256-bit frame capability plus a DISTINCT non-authorizing
//! `bridge_nonce` (bound into the cap) into [`Shared::screen_caps`].
//! Returns `{mount, bridge_nonce, generation, tag}` to the trusted host.
//!
//! `app_screen_consume {nonce}` — the SOLE authority is the one-use
//! FrameCap bearer plus the native operator peer guard
//! (`operator_connection`) that denies a registered-agent or detached
//! caller even when it somehow holds a valid nonce. The board's
//! headerless same-origin `GET /api/app-screen/<nonce>` navigation
//! relays ONLY the nonce — no cookie, key, or session field crosses.
//! Consume atomically BURNS the cap first, then re-proves: the stored
//! minting `session` id is still live in `Auth`, the installation's live
//! digest still equals the minted digest, the approval pin still holds,
//! and the package still validates — a revoke/upgrade between mint and
//! consume fails closed. The `bridge_nonce`/`generation`/`tag` rendered
//! into the bootstrap are the cap's stored values, never child-chosen.
//!
//! Token/key material is used only inside `Auth`; it is never stored in
//! a cap, logged, or pushed to the frame.

use std::time::Instant;

use serde_json::{json, Value};

use super::{required_str, Shared};
use crate::error::{Error, Result};
use crate::issue::app_screen_pkg;
use crate::operator_auth::Origin;

/// One outstanding frame capability. `session` is the VERIFIED minting
/// session's display id — resolved through `Auth` from the request's
/// real credential at mint time, never a caller field.
pub(crate) struct ScreenCap {
    pub install_id: String,
    pub digest: String,
    pub tag: String,
    /// Verified minting session's FULL credential hash (server-resolved
    /// at mint) — the cap's internal binding. Never the 8-hex display
    /// prefix, so a surviving prefix-colliding row cannot keep a revoked
    /// mint's cap live. Internal only — never rendered or transmitted.
    pub session: String,
    /// Host mount counter for this install — a superseded mount's mint
    /// is refused at consume.
    pub generation: u64,
    /// The DISTINCT non-authorizing bridge nonce minted here and bound
    /// into this cap; the host renders it into the bootstrap and the
    /// child echoes it in init. NOT the FrameCap bearer in the URL —
    /// the host recognizes its own mount's init by this stored value.
    pub bridge_nonce: String,
    pub issued: Instant,
}

/// Bounded capability map: ≤128 global, ≤4 per install, ≤8 per session.
pub(crate) const CAP_GLOBAL: usize = 128;
pub(crate) const CAP_PER_INSTALL: usize = 4;
pub(crate) const CAP_PER_SESSION: usize = 8;
/// One-use frame capability lifetime.
pub(crate) const CAP_TTL: std::time::Duration = std::time::Duration::from_secs(60);
/// Mint RATE bound — mints per verified session per rolling 60 s window.
/// This bounds the expensive digest/approval/package re-proof each mint
/// runs, distinct from the outstanding-cap count.
pub(crate) const MINT_RATE_PER_SESSION: usize = 64;
pub(crate) const MINT_RATE_WINDOW: std::time::Duration =
    std::time::Duration::from_secs(60);
/// Bound the rate map itself (a session-id → timestamps entry).
const MINT_RATE_SESSIONS: usize = 128;

/// A fresh 256-bit nonce as 64 lowercase hex chars (CSPRNG).
fn mint_nonce() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("getrandom");
    let mut out = String::with_capacity(64);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// The nonce grammar — exactly 64 lowercase hex, nothing else. Refuses
/// before any map lookup.
fn nonce_ok(nonce: &str) -> bool {
    nonce.len() == 64
        && nonce
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A stored minting-session hash is exactly a `sha256(token)` — 64
/// lowercase hex. Refuse anything else before trusting it as a key.
fn session_hash_ok(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Shared {
    /// Resolve a session's real bearer credential to its live
    /// `SessionView` through the daemon's own `Auth` — `token` is the
    /// request's session cookie value, `key` the `X-Cadence-Session`
    /// page key (absent on the public cookie-only surface), `origin` the
    /// request's classified origin. Returns `None` when the credential
    /// is absent, dead, revoked or malformed — never an error, so a
    /// lookup failure is indistinguishable from "no such session" and
    /// fails closed. The token is used only inside `Auth`, never stored.
    /// Resolve the request's real session bearer credential to a
    /// `(session, view)` pair: `session` is the verified row's FULL
    /// credential hash — the internal value the cap binds to (never the
    /// 8-hex display id, so a prefix collision with a surviving row
    /// cannot keep a revoked mint's cap live) — and `view` the public
    /// `SessionView` for the public-role check. Returns `None` when the
    /// credential is absent, dead, revoked or malformed. The token/key
    /// are consumed by `Auth` only — never stored here.
    fn session_for_credential(
        &self,
        token: &str,
        key: &str,
        origin: Origin,
    ) -> Option<(String, crate::operator_auth::SessionView)> {
        let now = self.operator_now();
        let mut auth = self.operator_auth();
        let hash = match origin {
            Origin::Public => auth.public_session_hash(token, now),
            _ => auth.session_hash(token, key, origin, now),
        }
        .ok()
        .flatten()?;
        // The same verified lookup for the public-role surface.
        let view = match origin {
            Origin::Public => auth.check_public(token, now),
            _ => auth.check(token, key, origin, now),
        }
        .ok()
        .flatten()?;
        Some((hash, view))
    }

    /// Whether the minting session is still live — re-verified in `Auth`
    /// at consume so a revoke between mint and consume kills the cap.
    /// `session_hash` is the cap's stored FULL credential hash; a
    /// different row sharing the 8-hex display prefix does not satisfy
    /// it. Fail-closed on any lookup error.
    fn session_id_live(&self, session_hash: &str) -> bool {
        if !session_hash_ok(session_hash) {
            return false;
        }
        let now = self.operator_now();
        let mut auth = self.operator_auth();
        auth.live_hash(session_hash, now).unwrap_or(false)
    }

    /// Rolling-window mint-rate check for one verified session. Sweeps
    /// timestamps older than the window; refuses when the session already
    /// minted [`MINT_RATE_PER_SESSION`] in it; otherwise records this mint.
    /// Runs BEFORE the expensive live digest/approval/package re-proof —
    /// the caller is already a proven operator + verified session, so the
    /// rate bound is the cheap gate. The map is bounded and swept.
    fn check_mint_rate(&self, session: &str) -> Result<()> {
        let now = Instant::now();
        let mut map = self
            .screen_mint_rate
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Sweep expired windows / empty entries so the map stays bounded.
        map.retain(|_, ts| {
            ts.retain(|t| now.duration_since(*t) < MINT_RATE_WINDOW);
            !ts.is_empty()
        });
        // A session not yet tracked may not be admitted if the map is
        // already at its session bound.
        if !map.contains_key(session) && map.len() >= MINT_RATE_SESSIONS {
            return Err(Error::rejected("screen mint rate map is full"));
        }
        let entry = map.entry(session.to_string()).or_default();
        if entry.len() >= MINT_RATE_PER_SESSION {
            return Err(Error::rejected(
                "screen mint rate exceeded — too many mounts in this window",
            ));
        }
        entry.push(now);
        Ok(())
    }

    /// Drop expired capabilities — the sweep mint runs before admit.
    fn sweep_screen_caps(&self) {
        let now = Instant::now();
        self.screen_caps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, cap| now.duration_since(cap.issued) < CAP_TTL);
    }

    /// `app_screen_mint {install_id, tag, token, key, origin,
    /// generation}` — a CLOSED param set: any other field refuses, so a
    /// forged authority field cannot ride a valid credential.
    /// `token`/`key`/`origin` are the consuming board request's own
    /// session credentials, relayed so the *daemon* proves them — never
    /// a caller-claimed session id. `generation` is the host's mount
    /// counter. Returns `{mount, bridge_nonce, generation, tag}`.
    pub(super) fn rpc_app_screen_mint(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("app screen mint", params, peer_pid)?;
        const ALLOWED: &[&str] =
            &["install_id", "tag", "token", "key", "origin", "generation"];
        let fields = params
            .as_object()
            .ok_or_else(|| Error::rejected("screen mint params must be an object"))?;
        if fields.keys().any(|k| !ALLOWED.contains(&k.as_str())) {
            return Err(Error::rejected("screen mint params have unsupported fields"));
        }
        let install_id = required_str(params, "install_id")?;
        let tag = required_str(params, "tag")?;
        let token = required_str(params, "token")?;
        let key = params.get("key").and_then(Value::as_str).unwrap_or("");
        // The origin MUST be a real classification — never default to
        // loopback on an unknown/missing value.
        let origin = match params.get("origin").and_then(Value::as_str) {
            Some("loopback") => Origin::Loopback,
            Some("tailnet") => Origin::Tailnet,
            Some("public") => Origin::Public,
            _ => {
                return Err(Error::rejected(
                    "screen mint needs a classified session origin",
                ))
            }
        };
        let generation = params
            .get("generation")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        // Verify the real credential natively; on the public surface the
        // session must also carry the operator role — a public `member`
        // session never mounts a screen.
        let (session, view) = self
            .session_for_credential(token, key, origin)
            .ok_or_else(|| {
                Error::rejected("screen mint needs a live operator session — sign in again")
            })?;
        if origin == Origin::Public {
            let operator = view
                .user
                .as_ref()
                .is_some_and(crate::operator_auth::BoardUser::is_operator);
            if !operator {
                return Err(Error::rejected(
                    "screen mint needs an operator-role public session",
                ));
            }
        }
        // `session` is the verified row's full credential hash — the
        // cap's internal binding. Bounded mint-rate gate BEFORE the expensive live digest/approval/
        // package re-proof — the caller is a proven operator + verified
        // session, so this cheap bound is what an abusive caller hits first.
        self.check_mint_rate(&session)?;
        let pm_dir = self.pm_dir()?;
        let pm = self.pm_at(&pm_dir)?;
        let (digest, _pkg) = self.screen_package_checked(&pm, install_id, tag)?;

        self.sweep_screen_caps();
        let mut caps = self.screen_caps.lock().unwrap_or_else(|e| e.into_inner());
        if caps.len() >= CAP_GLOBAL {
            return Err(Error::rejected("screen capability map is full"));
        }
        if caps.values().filter(|c| c.install_id == install_id).count() >= CAP_PER_INSTALL {
            return Err(Error::rejected(
                "too many live screen mounts for this installation",
            ));
        }
        if caps.values().filter(|c| c.session == session).count() >= CAP_PER_SESSION {
            return Err(Error::rejected(
                "too many live screen mounts for this session",
            ));
        }
        let nonce = mint_nonce();
        // The bridge nonce is minted WITH the cap (distinct bytes from
        // the FrameCap) and returned to the trusted host so it can render
        // it into the bootstrap and later recognize the echoing init.
        let bridge_nonce = mint_nonce();
        caps.insert(
            nonce.clone(),
            ScreenCap {
                install_id: install_id.to_string(),
                digest,
                tag: tag.to_string(),
                session,
                generation,
                bridge_nonce: bridge_nonce.clone(),
                issued: Instant::now(),
            },
        );
        Ok(json!({
            "mount": format!("/api/app-screen/{nonce}"),
            "bridge_nonce": bridge_nonce,
            "generation": generation,
            "tag": tag,
        }))
    }

    /// `app_screen_consume {nonce}` — a CLOSED param set: exactly
    /// `{nonce}`, nothing else. `operator_connection` peer guard denies a
    /// registered agent or detached caller even holding a valid nonce;
    /// then the cap is BURNED atomically before any further check
    /// (single-use), the stored minting session must still be live, and
    /// the live digest/approval/package re-proof runs. No cookie/key/
    /// session field is accepted or read.
    pub(super) fn rpc_app_screen_consume(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        // The native peer guard: an agent-derived or detached connection
        // is denied outright — a stolen nonce still cannot be spent by
        // one. This is what lets the headerless local GET stay safe.
        self.operator_connection("app screen consume", params, peer_pid)?;
        let fields = params
            .as_object()
            .ok_or_else(|| Error::rejected("screen consume params must be an object"))?;
        if fields.keys().any(|k| k != "nonce") {
            return Err(Error::rejected("screen consume admits only a nonce"));
        }
        let nonce = required_str(params, "nonce")?;
        if !nonce_ok(nonce) {
            return Err(Error::rejected("invalid screen capability"));
        }
        // Atomic burn — remove the cap before further verification, so a
        // second consume of the same nonce finds nothing (single-use).
        let cap = {
            let mut caps = self.screen_caps.lock().unwrap_or_else(|e| e.into_inner());
            match caps.remove(nonce) {
                Some(cap) => cap,
                None => return Err(Error::rejected("screen capability is spent or unknown")),
            }
        };
        if Instant::now().duration_since(cap.issued) >= CAP_TTL {
            return Err(Error::rejected("screen capability expired"));
        }
        // The minting session must still be live — a revoke between mint
        // and consume kills the cap. The id was server-resolved at mint.
        if !self.session_id_live(&cap.session) {
            return Err(Error::rejected("the minting session is no longer live"));
        }
        let pm_dir = self.pm_dir()?;
        let pm = self.pm_at(&pm_dir)?;
        // Re-verify the LIVE bundle digest + approval + package — a
        // revoke or upgrade between mint and consume refuses here.
        let (live_digest, pkg) = self.screen_package_checked(&pm, &cap.install_id, &cap.tag)?;
        if live_digest != cap.digest {
            return Err(Error::rejected(
                "installation digest changed since mint — mount refused",
            ));
        }
        Ok(json!({
            "tag": pkg.tag,
            "app": pkg.app,
            "digest": live_digest,
            "install_id": cap.install_id,
            "generation": cap.generation,
            "bridge_nonce": cap.bridge_nonce,
            "manifest": pkg.manifest,
            "assets": pkg.assets,
        }))
    }

    /// The shared live re-proof used by mint and consume: recompute the
    /// installed workspace bundle digest, require the approval pin,
    /// require the declared screen `app` to equal the installed app
    /// manifest's name, and re-run the package validator over the live
    /// members. Returns the live digest and the integrity-checked
    /// package.
    fn screen_package_checked(
        &self,
        pm: &crate::issue::Pm,
        install_id: &str,
        tag: &str,
    ) -> Result<(String, app_screen_pkg::ScreenPackage)> {
        use crate::issue::app_catalog::workspace;
        let store = &self.store;
        workspace::with_runtime_snapshot(pm, install_id, |row, files| {
            let live_digest = row["digest"]
                .as_str()
                .ok_or_else(|| Error::rejected("installation digest unavailable"))?
                .to_string();
            let status = store.app_capability_status(install_id, &live_digest)?;
            if status["state"].as_str() != Some("approved") {
                return Err(Error::rejected(
                    "installation is not approved at its current digest",
                ));
            }
            // The screen package's declared app must equal the installed
            // app manifest's name — a package cannot claim a different app.
            let installed_app = crate::issue::app::parse_manifest(
                files.get("app.md").ok_or_else(|| {
                    Error::rejected("installation manifest unavailable")
                })?,
            )?
            .app;
            let pkg = app_screen_pkg::extract(files, tag)?;
            if pkg.app != installed_app {
                return Err(Error::rejected(
                    "screen package declares an app that is not the installation",
                ));
            }
            Ok((live_digest, pkg))
        })
    }
}

/// Rendered-frame HTML: the static wrapper document carrying the CSP
/// nonce, the bootstrap JSON block, the approved CSS and the approved
/// IIFE — every byte host-verified so the package's own text can never
/// break out of its script/style context. Capped post-escape (768 KiB).
///
/// `csp_nonce` admits the two stamped blocks only; `bridge_nonce` is the
/// non-authorizing init token the child echoes (distinct from the
/// FrameCap bearer in the URL); `parent_origin` is the server-derived
/// exact board origin the child must `postMessage` to. The approved
/// IIFE may intentionally add code or read what it was granted — this
/// wrapper only prevents *accidental* display-injection and
/// unapproved-code execution; it does not constrain what approved code
/// deliberately does. The mount point is a generic `<div id="root">` —
/// the frozen app's own IIFE auto-mounts `#root`/`[data-sc-root]`; the
/// host emits no app-specific symbol.
pub fn render_frame_html(
    csp_nonce: &str,
    tag: &str,
    bridge_nonce: &str,
    generation: u64,
    css: &str,
    js: &str,
    parent_origin: &str,
) -> Result<String> {
    let boot = json!({
        "tag": tag,
        "bridge_nonce": bridge_nonce,
        "generation": generation,
        "parent_origin": parent_origin,
    })
    .to_string();
    let boot = escape_json_script(&boot);
    let js = check_script_text(js)?;
    let css = check_style_text(css)?;
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<script nonce=\"{csp}\" type=\"application/json\" id=\"cadence-screen-boot\">{boot}</script>\
<style>{css}</style>\
</head><body><div id=\"root\"></div>\
<script nonce=\"{csp}\">{js}</script>\
</body></html>",
        csp = csp_nonce,
        boot = boot,
        css = css,
        js = js,
    );
    if html.len() > 768 * 1024 {
        return Err(Error::rejected(
            "rendered screen document exceeds its bound",
        ));
    }
    Ok(html)
}

/// `<script type="application/json">`-safe escaping for the bootstrap:
/// JSON string escaping with `\\uXXXX` — `<`,`>`,`&` and the JS line
/// separators become `\\u003c`/`\\u003e`/`\\u0026`/`\\u2028`/`\\u2029`
/// literals the JSON parser restores, while the raw-text tokenizer
/// never sees a `</script`/`<!--` token. NOT HTML entities — inside a
/// raw-text `<script>` an `&lt;` reaches `JSON.parse` as a literal and
/// corrupts the field; `\\u003c` is the only correct form here.
fn escape_json_script(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            _ => out.push(c),
        }
    }
    out
}

/// Emit the approved IIFE source EXACTLY — a raw-text `<script>` block
/// cannot contain `</script`, `<!--` or `-->` (the HTML parser would
/// terminate the block or open a comment), so the mount FAILS CLOSED on
/// a case-insensitive match rather than rewriting bytes (a `\\u003c`
/// backslash-escape would corrupt operators/regex/React `</tag>`).
/// Preserving the exact approved source is what makes the approved
/// bytes' sha256 the executed bytes.
fn check_script_text(text: &str) -> Result<String> {
    let lower = text.to_ascii_lowercase();
    if lower.contains("</script") || lower.contains("<!--") || lower.contains("-->") {
        return Err(Error::rejected(
            "screen entry contains a forbidden HTML-script end token",
        ));
    }
    Ok(text.to_string())
}

/// Emit the approved CSS EXACTLY — a `<style>` raw-text block cannot
/// contain a case-insensitive `</style` end token; fail closed instead
/// of rewriting (a rewritten stylesheet is not the approved bytes).
fn check_style_text(text: &str) -> Result<String> {
    if text.to_ascii_lowercase().contains("</style") {
        return Err(Error::rejected(
            "screen stylesheet contains a forbidden </style end token",
        ));
    }
    Ok(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_preserves_exact_js_and_refuses_end_tokens() {
        let clean = "(function(){var r=1<2&&2>1;return r;})();";
        let h = render_frame_html("cspnonce", "main", "bridgenonce64", 1, "a{color:red}", clean, "http://h");
        assert!(h.is_ok());
        let h = h.unwrap();
        // Exact bytes preserved — verbatim, not backslash-rewritten.
        assert!(h.contains(clean));
        assert!(h.contains("<div id=\"root\"></div>"), "no generic #root mount: {h}");
        assert!(h.contains("cadence-screen-boot"));
        // Bootstrap uses \uXXXX not HTML entities.
        let h2 = render_frame_html("csp", "main", "b<nonce>", 1, "a{}", "x()", "http://h").unwrap();
        assert!(!h2.contains("&lt;"), "bootstrap used HTML entities: {h2}");
        assert!(h2.contains("\\u003c"), "bootstrap not \\uXXXX-escaped: {h2}");
        // End tokens refuse, never rewrite.
        for bad in ["a</script>b", "a<!--b", "a-->b", "a</ScRiPt>b"] {
            assert!(
                render_frame_html("csp", "m", "b", 1, "a{}", bad, "http://h").is_err(),
                "end-token IIFE admitted: {bad}"
            );
        }
        // CSS </style refuses.
        assert!(render_frame_html("csp", "m", "b", 1, "a</style>x", "x()", "http://h").is_err());
    }

    #[test]
    fn nonce_grammar_is_exact_64_lower_hex() {
        assert!(nonce_ok(&"a".repeat(64)));
        assert!(nonce_ok(&"0123456789abcdef".repeat(4)));
        assert!(!nonce_ok(&"a".repeat(63)));
        assert!(!nonce_ok(&"a".repeat(65)));
        assert!(!nonce_ok(&"A".repeat(64)));
        assert!(!nonce_ok(&"g".repeat(64)));
        assert!(!nonce_ok(""));
    }
}
