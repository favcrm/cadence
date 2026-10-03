//! Provider-owned normalization for the reviewed public Instagram source tool.
//! Worker text is never a source receipt: only the broker persists this output.
use serde_json::{json, Value};

const MAX_POSTS: usize = 24;
const MAX_CAPTION_BYTES: usize = 8 * 1024;

pub(super) fn valid_handle(handle: &str) -> bool {
    let bytes = handle.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 30
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'.'))
}

fn safe_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 40
        && code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// Preview media is an untrusted display hint, never a durable generated asset.
/// Restrict browser image loads to Instagram-owned CDN hosts; do not fetch these
/// URLs on the daemon or treat them as the reviewed source's durable bytes.
fn preview_url(value: &str) -> Option<&str> {
    let uri: ureq::http::Uri = value.parse().ok()?;
    if uri.scheme_str() != Some("https") || uri.authority()?.as_str().contains('@') {
        return None;
    }
    let host = uri.host()?.to_ascii_lowercase();
    if host == "cdninstagram.com"
        || host.ends_with(".cdninstagram.com")
        || host == "fbcdn.net"
        || host.ends_with(".fbcdn.net")
    {
        Some(value)
    } else {
        None
    }
}

fn verified_profile(user: &Value, handle: &str) -> Result<(), String> {
    let user = user
        .as_object()
        .ok_or("source profile identity is malformed")?;
    if user.get("is_private") != Some(&Value::Bool(false)) {
        return Err("source profile is private or privacy is unknown".into());
    }
    if !user
        .get("username")
        .and_then(Value::as_str)
        .is_some_and(|found| found.eq_ignore_ascii_case(handle))
    {
        return Err("source post profile identity changed".into());
    }
    Ok(())
}

pub(super) fn normalize_posts(handle: &str, result: &Value) -> Result<Value, String> {
    if !valid_handle(handle) {
        return Err("source handle is invalid".into());
    }
    let body = result
        .as_object()
        .ok_or("source provider result is not an object")?;
    if body.get("success") != Some(&Value::Bool(true))
        || body
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| status != "ok")
    {
        return Err("public source lookup was not successful".into());
    }
    let items = body
        .get("items")
        .and_then(Value::as_array)
        .ok_or("source post list is missing")?;
    // The published full response includes a top-level user and each post's
    // user. The catalog example truncates posts, so validate every identity
    // supplied and require at least one attested account for each post.
    let top_user = body.get("user");
    if let Some(user) = top_user {
        verified_profile(user, handle)?;
    }
    let mut posts = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for row in items.iter().take(MAX_POSTS) {
        if let Some(user) = row.get("user") {
            verified_profile(user, handle)?;
        } else if top_user.is_none() {
            return Err("source post profile is missing".into());
        }
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 100)
            .ok_or("source post identity is malformed")?;
        if !seen.insert(id) {
            // The provider may repeat a pinned post in the ordinary grid.
            // Retain the first attested copy and never invent a second post.
            continue;
        }
        let code = row
            .get("code")
            .or_else(|| row.get("shortcode"))
            .and_then(Value::as_str)
            .filter(|code| safe_code(code))
            .ok_or("source post permalink is malformed")?;
        let caption = row
            .get("caption")
            .and_then(|caption| {
                caption
                    .get("text")
                    .or_else(|| caption.as_str().map(|_| caption))
            })
            .and_then(Value::as_str)
            .unwrap_or("");
        if caption.len() > MAX_CAPTION_BYTES || caption.chars().any(|ch| ch == '\0') {
            return Err("source post caption exceeds the supported bound".into());
        }
        let created_at = row
            .get("created_at")
            .and_then(Value::as_str)
            .filter(|value| {
                value.len() >= 20
                    && value.len() <= 40
                    && value.ends_with('Z')
                    && value.as_bytes().get(10) == Some(&b'T')
            });
        let taken_at = row
            .get("taken_at")
            .and_then(Value::as_i64)
            .filter(|value| (946684800..4102444800).contains(value));
        if created_at.is_none() && taken_at.is_none() {
            return Err("source post time is missing".into());
        }
        let image_url = row
            .pointer("/image_versions2/candidates/0/url")
            .and_then(Value::as_str)
            .and_then(preview_url)
            .or_else(|| {
                row.get("display_uri")
                    .and_then(Value::as_str)
                    .and_then(preview_url)
            });
        let kind = match row.get("media_type").and_then(Value::as_u64) {
            Some(1) => "image",
            Some(2) => "video",
            Some(8) => "carousel",
            _ => "unknown",
        };
        posts.push(json!({
            "id": id,
            "caption": caption,
            "permalink": format!("https://www.instagram.com/p/{code}/"),
            "published_at": created_at,
            "published_at_unix": taken_at,
            "media_kind": kind,
            "preview_url": image_url,
        }));
    }
    let profile_verified = top_user.is_some() || !posts.is_empty();
    Ok(json!({
        "schema": 1,
        "kind": "social.source.posts",
        "provider": "agenticos_external",
        "source_tool": super::POSTS_TOOL,
        "handle": handle,
        "profile_verified": profile_verified,
        "empty_reason": if posts.is_empty() { Some("no_public_posts_or_unavailable") } else { None },
        "posts": posts,
        "more_available": body.get("more_available").and_then(Value::as_bool).unwrap_or(false) || items.len() > MAX_POSTS,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply() -> Value {
        json!({"success":true,"status":"ok","user":{"username":"juicysuite_crm","is_private":false},"items":[
            {"id":"123","code":"D7_aB-2","user":{"username":"juicysuite_crm","is_private":false},"caption":{"text":"Original facts"},"created_at":"2026-09-27T01:00:00Z","media_type":1,"image_versions2":{"candidates":[{"url":"https://scontent.cdninstagram.com/image.jpg"}]}},
            {"id":"456","code":"D7CD","user":{"username":"juicysuite_crm","is_private":false},"caption":null,"taken_at":1790470800,"media_type":8,"image_versions2":{"candidates":[{"url":"http://127.0.0.1/private"}]}}
        ]})
    }

    #[test]
    fn public_posts_are_bounded_and_retained_as_provider_receipts() {
        let page = normalize_posts("juicysuite_crm", &reply()).unwrap();
        assert_eq!(
            page["posts"][0]["permalink"],
            "https://www.instagram.com/p/D7_aB-2/"
        );
        assert_eq!(page["posts"][0]["caption"], "Original facts");
        assert_eq!(
            page["posts"][0]["preview_url"],
            "https://scontent.cdninstagram.com/image.jpg"
        );
        assert!(page["posts"][1]["preview_url"].is_null());
        assert_eq!(page["posts"][1]["media_kind"], "carousel");
        assert_eq!(page["handle"], "juicysuite_crm");
        assert_eq!(page["profile_verified"], true);
    }

    #[test]
    fn top_level_profile_attests_posts_when_catalog_omits_nested_user() {
        let mut page = reply();
        page["items"][0].as_object_mut().unwrap().remove("user");
        page["items"][0]["image_versions2"]["candidates"][0]["url"] =
            json!("http://127.0.0.1/private");
        page["items"][0]["display_uri"] = json!("https://instagram.fbcdn.net/display.jpg");
        let normalized = normalize_posts("juicysuite_crm", &page).unwrap();
        assert_eq!(normalized["profile_verified"], true);
        assert_eq!(
            normalized["posts"][0]["preview_url"],
            "https://instagram.fbcdn.net/display.jpg"
        );
        page["user"]["username"] = json!("other_brand");
        assert!(normalize_posts("juicysuite_crm", &page).is_err());
        page["user"]["username"] = json!("juicysuite_crm");
        page["user"]["is_private"] = json!(true);
        assert!(normalize_posts("juicysuite_crm", &page).is_err());
        page["user"].as_object_mut().unwrap().remove("is_private");
        assert!(normalize_posts("juicysuite_crm", &page).is_err());
        page["user"]["is_private"] = json!(false);
        page.as_object_mut().unwrap().remove("user");
        assert!(normalize_posts("juicysuite_crm", &page).is_err());
    }

    #[test]
    fn forged_resource_and_private_source_fail_closed() {
        let mut page = reply();
        page["items"][0]["user"]["username"] = json!("another_company");
        assert!(normalize_posts("juicysuite_crm", &page).is_err());
        page["items"][0]["user"]["username"] = json!("juicysuite_crm");
        page["items"][0]["user"]["is_private"] = json!(true);
        assert!(normalize_posts("juicysuite_crm", &page).is_err());
        page["items"][0]["user"]["is_private"] = json!(false);
        page["items"][0]["code"] = json!("../internal");
        assert!(normalize_posts("juicysuite_crm", &page).is_err());
        page["items"][0]["code"] = json!("D7_aB-2");
        page["items"][1]["id"] = json!("123");
        assert_eq!(
            normalize_posts("juicysuite_crm", &page).unwrap()["posts"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn empty_and_bad_provider_results_do_not_become_sample_posts() {
        let mut page = reply();
        page["items"] = json!([]);
        let empty = normalize_posts("juicysuite_crm", &page).unwrap();
        assert!(empty["posts"].as_array().unwrap().is_empty());
        assert_eq!(empty["profile_verified"], true);
        assert_eq!(empty["empty_reason"], "no_public_posts_or_unavailable");
        page.as_object_mut().unwrap().remove("user");
        assert_eq!(
            normalize_posts("juicysuite_crm", &page).unwrap()["profile_verified"],
            false
        );
        page["status"] = json!("rate_limited");
        assert!(normalize_posts("juicysuite_crm", &page).is_err());
        page["status"] = json!("ok");
        page["success"] = json!(false);
        assert!(normalize_posts("juicysuite_crm", &page).is_err());
        assert!(!valid_handle("juicysuite_crm/../../private"));
    }
}
